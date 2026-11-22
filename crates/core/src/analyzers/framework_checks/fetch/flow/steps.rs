use autoreview_dataflow::cfg::IdentityOperand;
use autoreview_dataflow::{CfgNode, Stmt};

use super::super::syntax::{Binding, Call, Condition, Syntax};

#[derive(Clone, Copy)]
pub(super) enum Operand<'s> {
    Binding(Binding<'s>),
    Result(usize),
    Boolean(bool),
}

pub(super) enum Step<'s> {
    Call(usize, Call<'s>),
    Bind(Binding<'s>, Option<Operand<'s>>),
    Return(Operand<'s>),
    ReturnedPromise(usize, usize),
}

pub(super) struct Block<'s> {
    pub steps: Vec<Step<'s>>,
    pub condition: Option<Condition<'s>>,
}

impl<'s> Block<'s> {
    pub(super) fn lower(node: &CfgNode<Stmt>, syntax: &Syntax<'_, 's, '_>) -> Self {
        let steps = node
            .stmts
            .iter()
            .filter_map(|stmt| lower_step(stmt, node, syntax))
            .collect();
        let condition = node
            .condition_span()
            .and_then(|span| syntax.condition(span));
        Self { steps, condition }
    }
}

fn lower_step<'s>(
    stmt: &Stmt,
    node: &CfgNode<Stmt>,
    syntax: &Syntax<'_, 's, '_>,
) -> Option<Step<'s>> {
    match stmt {
        Stmt::Call {
            site: Some(site), ..
        } => Some(Step::Call(site.start, syntax.operation(site.start)?)),
        Stmt::Rebind(write) => {
            let (name, position) = write.target();
            let target = syntax.bind_at(name, position)?;
            let rhs = match write.value() {
                Some(IdentityOperand::Binding(name)) => {
                    syntax.bind_at(name, position).map(Operand::Binding)
                }
                Some(IdentityOperand::CallResult { start, .. }) => Some(Operand::Result(*start)),
                Some(IdentityOperand::Boolean(value)) => Some(Operand::Boolean(*value)),
                _ => None,
            };
            Some(Step::Bind(target, rhs))
        }
        Stmt::ReturnIdentity(value) => lower_return(value.as_ref(), node.return_start(), syntax),
        _ => None,
    }
}

fn lower_return<'s>(
    operand: Option<&IdentityOperand>,
    start: Option<usize>,
    syntax: &Syntax<'_, 's, '_>,
) -> Option<Step<'s>> {
    let location = start?;
    if let Some((read, fetch)) = syntax.returned_promise(location) {
        return Some(Step::ReturnedPromise(read, fetch));
    }
    let value = match operand? {
        IdentityOperand::CallResult { start, .. } => Operand::Result(*start),
        IdentityOperand::Binding(name) => Operand::Binding(syntax.return_binding(name, location)?),
        _ => return None,
    };
    Some(Step::Return(value))
}
