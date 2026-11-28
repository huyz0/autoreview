use super::{from_value, outermost_group, Bindings, Lower, Mode, Node, NodeId};
use crate::cfg::{BindingWrite, Stmt};
use crate::lower::{keyed_syntax, record_evaluation};
use crate::lower::rust::{destructuring, line, simple_binding};

pub(in crate::lower::rust) struct Assignment<'a> {
    declaration: Node<'a>,
    local: Option<&'a str>,
}

pub(super) fn is_initializer(node: Node<'_>, source: &[u8]) -> bool {
    let value = outermost_group(node);
    value.parent().is_some_and(|parent| {
        parent.kind() == "let_declaration"
            && parent.child_by_field_name("value") == Some(value)
            && (simple_binding(parent)
                || destructuring::declaration_bindings(parent, source).is_some()
                || (value != node && parent.child_by_field_name("alternative").is_some()))
    })
}

pub(super) fn is_reassignment(node: Node<'_>) -> bool {
    let value = outermost_group(node);
    value.parent().is_some_and(|assignment| assignment_target(assignment) && assignment.child_by_field_name("right") == Some(value))
}

fn assignment_target(node: Node<'_>) -> bool {
    node.kind() == "assignment_expression" && node.child_by_field_name("left").is_some_and(|target| target.kind() == "identifier")
}

impl Lower<'_> {
    pub(in crate::lower::rust) fn lower_assignment(&mut self, node: Node<'_>, at: NodeId, names: &Bindings) -> Option<NodeId> {
        let target = node.child_by_field_name("left").and_then(|target| self.variable(target, names));
        let right = node.child_by_field_name("right");
        if let Some(branch) = right.and_then(from_value).filter(|_| assignment_target(node)) {
            return self.lower_branch_value(branch, at, names, &Mode::Assign(Assignment {
                declaration: node,
                local: target.as_deref(),
            }));
        }
        let end = self.value(right, at, names, target)?;
        record_evaluation(&mut self.cfg, end, node);
        self.keyed_write(node, end);
        Some(end)
    }

    pub(in crate::lower::rust) fn lower_local(&mut self, stmt: Node<'_>, at: NodeId, names: &mut Bindings) -> Option<NodeId> {
        if let Some(bindings) = destructuring::declaration_bindings(stmt, self.source) {
            return self.destructured_declaration(stmt, at, names, bindings);
        }
        self.binding(stmt, at, names)
    }

    fn binding(
        &mut self,
        stmt: Node<'_>,
        at: NodeId,
        names: &mut Bindings,
    ) -> Option<NodeId> {
        let pattern = stmt.child_by_field_name("pattern");
        let name = pattern
            .filter(|n| n.kind() == "identifier")
            .map(|n| self.text(n).to_owned());
        let target = name.as_ref().map(|name| self.fresh(name));
        let value = stmt.child_by_field_name("value");
        let at = match value.and_then(from_value) {
            Some(value) => {
                let mode = Mode::Assign(Assignment {
                    declaration: stmt,
                    local: target.as_deref(),
                });
                self.lower_branch_value(value, at, names, &mode)?
            }
            None => {
                let end = self.value(value, at, names, target.clone())?;
                record_evaluation(&mut self.cfg, end, stmt);
                self.keyed_write(stmt, end);
                end
            }
        };
        if let Some((name, target)) = name.zip(target) {
            names.insert(name, target);
        }
        Some(at)
    }

    pub(super) fn assign_arm(
        &mut self,
        value: Option<Node<'_>>,
        at: NodeId,
        names: &Bindings,
        assignment: &Assignment<'_>,
    ) -> Option<NodeId> {
        let declaration = assignment.declaration;
        let end = self.value(value, at, names, assignment.local.map(str::to_owned))?;
        if let Some(value) = value {
            record_evaluation(&mut self.cfg, end, value);
            if let Some(evaluation) = self.cfg.nodes[end].evaluations.last_mut() {
                evaluation.assignment = Some((declaration.byte_range(), declaration.kind_id()));
            }
        }
        if let Some(target) = declaration
            .child_by_field_name("pattern").or_else(|| declaration.child_by_field_name("left"))
            .filter(|target| target.kind() == "identifier" && self.cfg.keyed_flow.is_some())
        {
            let name = self.text(target).to_owned();
            let value =
                value.and_then(|value| keyed_syntax::result_operand(value, self.source, true));
            self.cfg.emit(
                end,
                Stmt::Rebind(BindingWrite {
                    name,
                    start: if declaration.kind() == "assignment_expression" { target.start_byte() } else { declaration.start_byte() },
                    value,
                }),
                line(declaration),
            );
        }
        Some(end)
    }
}
