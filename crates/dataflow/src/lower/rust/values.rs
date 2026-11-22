use super::{line, Bindings, Lower};
use crate::cfg::{CallTarget, EdgeKind, NodeId, RhsShape, Stmt};
use tree_sitter::Node;

impl Lower<'_> {
    pub(super) fn value(
        &mut self,
        expr: Option<Node<'_>>,
        at: NodeId,
        names: &Bindings,
        target: Option<String>,
    ) -> NodeId {
        let Some(expr) = expr.map(unwrap_value) else {
            return at;
        };
        match expr.kind() {
            "try_expression" => self.try_value(expr, at, names, target),
            "call_expression" => self.call(expr, at, names, target),
            _ => {
                if let Some(lhs) = target {
                    let rhs = self
                        .variable(expr, names)
                        .map_or(RhsShape::Unknown, RhsShape::Var);
                    self.emit(at, line(expr), Stmt::Assign { lhs, rhs });
                }
                at
            }
        }
    }

    fn try_value(
        &mut self,
        expr: Node<'_>,
        at: NodeId,
        names: &Bindings,
        target: Option<String>,
    ) -> NodeId {
        let evaluated = self.value(expr.named_child(0), at, names, target);
        let early_return = self.cfg.push_node(line(expr));
        let success = self.cfg.push_node(line(expr));
        self.edge(evaluated, early_return, EdgeKind::False);
        self.edge(evaluated, success, EdgeKind::True);
        self.finish(early_return, line(expr));
        success
    }

    fn call(
        &mut self,
        expr: Node<'_>,
        mut at: NodeId,
        names: &Bindings,
        target: Option<String>,
    ) -> NodeId {
        let Some(function) = expr.child_by_field_name("function") else {
            return at;
        };
        let function = function.child_by_field_name("function").unwrap_or(function);
        let callee = self.call_name(function, names, &mut at);
        let mut args = Vec::new();
        if let Some(arguments) = expr.child_by_field_name("arguments") {
            for argument in arguments
                .named_children(&mut arguments.walk())
                .filter(|part| !matches!(part.kind(), "line_comment" | "block_comment"))
            {
                if let Some(variable) = self.variable(argument, names) {
                    args.push(variable);
                } else {
                    let temporary = self.fresh("expression");
                    at = self.value(Some(argument), at, names, Some(temporary.clone()));
                    args.push(temporary);
                }
            }
        }
        self.emit(
            at,
            line(expr),
            Stmt::Call {
                target: callee,
                args,
                assigned_to: target,
                site: crate::lower::site::capture(expr, self.source),
            },
        );
        at
    }

    fn call_name(&mut self, function: Node<'_>, names: &Bindings, at: &mut NodeId) -> CallTarget {
        match function.kind() {
            "identifier" | "scoped_identifier" => CallTarget::Named(
                self.text(function)
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect(),
            ),
            "field_expression" => {
                let receiver = function.child_by_field_name("value");
                *at = self.value(receiver, *at, names, None);
                function
                    .child_by_field_name("field")
                    .map_or(CallTarget::Unknown, |field| {
                        let object = receiver.map_or("", |node| self.text(node));
                        CallTarget::Named(format!("{object}.{}", self.text(field)))
                    })
            }
            _ => CallTarget::Unknown,
        }
    }
}

pub(super) fn unwrap_value(mut expr: Node<'_>) -> Node<'_> {
    while matches!(
        expr.kind(),
        "parenthesized_expression" | "await_expression" | "reference_expression"
    ) {
        let Some(inner) = expr
            .child_by_field_name("value")
            .or_else(|| expr.named_child(0))
        else {
            break;
        };
        expr = inner;
    }
    expr
}
