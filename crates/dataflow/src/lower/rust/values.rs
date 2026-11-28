use super::{argument_flow, branch_value::{self, Mode}, element_access, line, Bindings, Lower};
use crate::cfg::{CallSite, CallTarget, EdgeKind, IdentityOperand, NodeId, RhsShape, Stmt};
use crate::lower::site::{self, children};
use tree_sitter::Node;

impl Lower<'_> {
    pub(super) fn value(
        &mut self,
        expr: Option<Node<'_>>,
        at: NodeId,
        names: &Bindings,
        target: Option<String>,
    ) -> Option<NodeId> {
        if let Some(awaited) = expr.and_then(keyed_await).filter(|_| self.cfg.keyed_flow.is_some()) {
            if let Some(await_call) = crate::lower::site::await_binding(awaited, self.source) {
                self.cfg.emit(at, await_call, line(awaited));
            }
        }
        let Some(expr) = expr.map(unwrap_value) else {
            return Some(at);
        };
        match expr.kind() {
            "block" if super::is_labeled_block(expr) => self.labeled_block(expr, at, names, target),
            "try_expression" => self.try_value(expr, at, names, target),
            "call_expression" => self.call(expr, at, names, target),
            _ => {
                let at = if let Some(operands) = eager_operands(expr) {
                    self.eager_values(expr, operands, at, names)?
                } else { at };
                if let Some(lhs) = target {
                    let rhs = self
                        .variable(expr, names)
                        .map_or(RhsShape::Unknown, RhsShape::Var);
                    self.cfg.emit(at, Stmt::Assign { lhs, rhs }, line(expr));
                }
                Some(at)
            }
        }
    }

    fn eager_values<'tree>(&mut self, expr: Node<'_>, mut operands: impl Iterator<Item = Node<'tree>>, mut at: NodeId, names: &Bindings) -> Option<NodeId> {
        let staged = argument_flow::stages_aggregate(expr);
        let projection_owner = (!staged && self.cfg.keyed_flow.is_some())
            .then(|| element_access::projection_owner(expr)).flatten();
        while let Some(operand) = operands.next() {
            let end = if staged {
                self.lower_operand(operand, at, names, true).map(|(end, _)| end)
            } else {
                self.value(Some(operand), at, names, None)
            };
            let Some(end) = end else {
                self.skip_calls(operands);
                return None;
            };
            if projection_owner.is_some() {
                self.record_operand(operand, operand, true, projection_owner, end);
            }
            at = end;
        }
        Some(at)
    }

    fn try_value(
        &mut self,
        expr: Node<'_>,
        at: NodeId,
        names: &Bindings,
        target: Option<String>,
    ) -> Option<NodeId> {
        let evaluated = self.value(children(expr).next(), at, names, target)?;
        let early_return = self.cfg.push_node(line(expr));
        let success = self.cfg.push_node(line(expr));
        self.edge(evaluated, early_return, EdgeKind::False);
        self.edge(evaluated, success, EdgeKind::True);
        self.finish(early_return, line(expr));
        Some(success)
    }

    fn call(
        &mut self,
        expr: Node<'_>,
        mut at: NodeId,
        names: &Bindings,
        target: Option<String>,
    ) -> Option<NodeId> {
        let Some(function) = expr.child_by_field_name("function") else {
            return Some(at);
        };
        let function = function.child_by_field_name("function").unwrap_or(function);
        let staged = argument_flow::stages_call(expr);
        let mut site = site::capture(expr, self.source);
        let mut arguments = expr.child_by_field_name("arguments").into_iter().flat_map(children);
        let Some(callee) = self.call_name(function, names, &mut at, staged, site.as_deref_mut()) else {
            self.skip_call(expr, arguments);
            return None;
        };
        let mut args = Vec::new();
        while let Some(argument) = arguments.next() {
            let Some((end, argument)) = self.lower_operand(argument, at, names, staged) else {
                self.skip_call(expr, arguments);
                return None;
            };
            at = end;
            if let Some(operand) = site.as_mut().filter(|_| staged).and_then(|call| call.arguments.get_mut(args.len())) {
                *operand = Some(IdentityOperand::Binding(argument.clone()));
            }
            args.push(argument);
        }
        self.cfg.emit(
            at,
            Stmt::Call {
                target: callee,
                args,
                assigned_to: target,
                site,
            },
            line(expr),
        );
        Some(at)
    }

    fn skip_call<'tree>(&mut self, call: Node<'_>, arguments: impl IntoIterator<Item = Node<'tree>>) {
        if self.cfg.keyed_flow.is_some() {
            self.cfg.unreachable_calls.push(call.start_byte());
            self.skip_calls(arguments);
        }
    }

    fn lower_operand(&mut self, operand: Node<'_>, at: NodeId, names: &Bindings, staged: bool) -> Option<(NodeId, String)> {
        if !staged {
            if let Some(variable) = self.variable(operand, names) {
                return Some((at, variable));
            }
        }
        let temporary = self.fresh(if staged { argument_flow::OPERAND_TEMPORARY } else { "expression" });
        let end = if let Some(branch) = branch_value::from_value(operand) {
            self.lower_branch_value(branch, at, names, &Mode::Operand { syntax: operand, temporary: &temporary })?
        } else {
            let end = self.value(Some(operand), at, names, Some(temporary.clone()))?;
            if staged {
                self.capture_operand(operand, Some(operand), &temporary, end);
            }
            end
        };
        Some((end, temporary))
    }

    fn call_name(&mut self, function: Node<'_>, names: &Bindings, at: &mut NodeId, staged: bool, site: Option<&mut CallSite>) -> Option<CallTarget> {
        Some(match function.kind() {
            "identifier" | "scoped_identifier" => CallTarget::Named(
                self.text(function)
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect(),
            ),
            "field_expression" => {
                let receiver = function.child_by_field_name("value");
                *at = self.value(receiver, *at, names, None)?;
                if let Some(receiver) = receiver.filter(|_| staged) {
                    let temporary = self.fresh(argument_flow::OPERAND_TEMPORARY);
                    self.capture_operand(receiver, Some(receiver), &temporary, *at);
                    if let Some(site) = site.filter(|call| call.receiver.is_some()) {
                        site.receiver = Some(temporary);
                    }
                }
                function
                    .child_by_field_name("field")
                    .map_or(CallTarget::Unknown, |field| {
                        let object = receiver.map_or("", |node| self.text(node));
                        CallTarget::Named(format!("{object}.{}", self.text(field)))
                    })
            }
            _ => CallTarget::Unknown,
        })
    }
}

pub(super) fn eager_operands(node: Node<'_>) -> Option<impl Iterator<Item = Node<'_>>> {
    let aggregate = argument_flow::aggregate_operands(node);
    let plain = if aggregate.is_some() {
        None
    } else {
        let single_operand = match node.kind() {
            "field_expression" | "type_cast_expression" => Some(node.child_by_field_name("value")?),
            _ if evaluates_all_operands(node) => None,
            _ => return None,
        };
        Some(children(node).filter(move |operand| single_operand.is_none_or(|value| *operand == value)))
    };
    Some(aggregate.into_iter().flatten().chain(plain.into_iter().flatten()))
}

fn evaluates_all_operands(node: Node<'_>) -> bool {
    match node.kind() {
        "index_expression" | "range_expression" => true,
        "unary_expression" => node.children(&mut node.walk()).any(|part| matches!(part.kind(), "-" | "!" | "*")),
        "binary_expression" => node.child_by_field_name("operator").is_some_and(|operator| {
            !matches!(operator.kind(), "&&" | "||")
        }),
        _ => false,
    }
}

fn keyed_await(mut expression: Node<'_>) -> Option<Node<'_>> {
    while expression.kind() == "parenthesized_expression" {
        expression = expression.named_child(0)?;
    }
    (expression.kind() == "await_expression").then_some(expression)
}

pub(super) fn unwrap_value(mut expr: Node<'_>) -> Node<'_> {
    while matches!(
        expr.kind(),
        "parenthesized_expression" | "await_expression" | "reference_expression"
    ) {
        let Some(inner) = expr
            .child_by_field_name("value")
            .or_else(|| children(expr).next())
        else {
            break;
        };
        expr = inner;
    }
    expr
}
