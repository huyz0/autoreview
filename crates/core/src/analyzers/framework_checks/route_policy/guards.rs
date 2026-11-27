use std::collections::BTreeMap;

use super::super::web::node_calls::{string_literal, NodeCalls};
use super::super::web::string_flow::meaningful_children;
use super::super::{children, text, Node};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Key<'s> {
    Literal(&'s str),
    Binding(usize),
}

pub(super) struct Check<'a, 't> {
    pub calls: &'a BTreeMap<usize, Node<'t>>,
    pub key_argument: usize,
}

pub(super) enum ResultUse {
    DenialGuard,
    DirectCall,
}

pub(super) struct Handler<'a, 't, 's> {
    body: Node<'t>,
    calls: &'a NodeCalls<'t, 's>,
    source: &'s str,
}

impl<'a, 't, 's> Handler<'a, 't, 's> {
    pub(super) fn new(
        handler: Node<'t>,
        calls: &'a NodeCalls<'t, 's>,
        source: &'s str,
    ) -> Option<Self> {
        let body = handler.child_by_field_name("body")?;
        (body.kind() == "statement_block" && !body.has_error()).then_some(Self {
            body,
            calls,
            source,
        })
    }

    pub(super) fn direct_calls(&self) -> impl Iterator<Item = Node<'t>> {
        children(self.body).filter_map(direct_call)
    }

    pub(super) fn key(&self, call: Node<'t>, index: usize) -> Option<Key<'s>> {
        let argument = argument(call, index)?;
        if let Some(literal) = string_literal(argument, self.source) {
            return Some(Key::Literal(literal));
        }
        let binding = self.calls.const_initializer(argument)?;
        let declaration = binding.parent()?;
        (declaration.parent()?.parent() == Some(self.body))
            .then_some(Key::Binding(declaration.start_byte()))
    }

    pub(super) fn prior_check(
        &self,
        target: Node<'t>,
        key: Key<'s>,
        check: &Check<'_, 't>,
        result_use: ResultUse,
    ) -> Option<Node<'t>> {
        children(self.body)
            .take_while(|statement| statement.end_byte() <= target.start_byte())
            .filter_map(|statement| match result_use {
                ResultUse::DenialGuard => self.negated_guard(statement),
                ResultUse::DirectCall => direct_call(statement),
            })
            .find(|call| {
                check.calls.contains_key(&call.start_byte())
                    && self.key(*call, check.key_argument) == Some(key)
            })
    }

    fn negated_guard(&self, statement: Node<'t>) -> Option<Node<'t>> {
        if statement.kind() != "if_statement"
            || !exits(statement.child_by_field_name("consequence")?)
        {
            return None;
        }
        let condition = unparenthesize(statement.child_by_field_name("condition")?)?;
        if condition.kind() != "unary_expression"
            || text(condition.child_by_field_name("operator")?, self.source) != "!"
        {
            return None;
        }
        let value = unparenthesize(condition.child_by_field_name("argument")?)?;
        let value = if value.kind() == "identifier" {
            self.calls.const_initializer(value)?
        } else {
            value
        };
        awaited_call(value)
    }
}

pub(super) fn argument(call: Node<'_>, index: usize) -> Option<Node<'_>> {
    let mut arguments = meaningful_children(call.child_by_field_name("arguments")?);
    let selected = arguments
        .by_ref()
        .take(index)
        .try_fold((), |(), arg| (arg.kind() != "spread_element").then_some(()));
    selected?;
    arguments
        .next()
        .filter(|node| node.kind() != "spread_element")
}

fn unparenthesize(mut node: Node<'_>) -> Option<Node<'_>> {
    while node.kind() == "parenthesized_expression" {
        node = meaningful_children(node).next()?;
    }
    Some(node)
}

fn exits(statement: Node<'_>) -> bool {
    if statement.kind() != "statement_block" {
        return matches!(statement.kind(), "return_statement" | "throw_statement");
    }
    let mut statements = meaningful_children(statement);
    let Some(exit) = statements.next() else {
        return false;
    };
    statements.next().is_none() && matches!(exit.kind(), "return_statement" | "throw_statement")
}

fn direct_call(statement: Node<'_>) -> Option<Node<'_>> {
    let expr = match statement.kind() {
        "expression_statement" | "return_statement" => meaningful_children(statement).next()?,
        "lexical_declaration" => {
            let mut declarations = meaningful_children(statement);
            let declaration = declarations.next()?;
            if declarations.next().is_some() {
                return None;
            }
            declaration.child_by_field_name("value")?
        }
        _ => return None,
    };
    let expr = unparenthesize(expr)?;
    if expr.kind() == "call_expression" {
        Some(expr)
    } else {
        awaited_call(expr)
    }
}

fn awaited_call(expr: Node<'_>) -> Option<Node<'_>> {
    let expr = unparenthesize(expr)?;
    let call = (expr.kind() == "await_expression")
        .then(|| meaningful_children(expr).next())
        .flatten()?;
    let call = unparenthesize(call)?;
    (call.kind() == "call_expression").then_some(call)
}
