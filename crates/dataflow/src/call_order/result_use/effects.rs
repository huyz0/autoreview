use std::collections::HashSet;

use tree_sitter::Node;

use crate::cfg::Evaluation;
use crate::lower::{keyed_syntax::assignment_parts, site};

use super::super::{plan::Plan, ContractLanguage, KeyedFunction};
use super::references::{captured_names, walk_names};

#[derive(Clone)]
pub(super) struct Origin {
    pub(super) call: String,
    pub(super) start: usize,
    pub(super) line: u32,
}

pub(super) enum Value {
    Source(Origin),
    Copy(usize),
    Other,
}

pub(super) struct Write {
    pub(super) target: Option<usize>,
    pub(super) value: Value,
}

pub(super) enum Effect {
    Read(usize),
    Write(Vec<Write>),
}

pub(super) fn lower<'src>(
    plan: &Plan<'_, 'src>,
    function: &KeyedFunction<'_, '_>,
    source: &'src [u8],
    reachable: &[bool],
) -> Vec<Vec<Effect>> {
    let language = plan.spec.contract.as_ref().unwrap().language;
    let flow = Lower { plan, source, language, captured: captured_names(function.node, source, language) };
    function
        .graph
        .nodes
        .iter()
        .zip(reachable)
        .map(|(node, &reached)| {
            let mut effects = Vec::new();
            if reached {
                for range in &node.evaluations {
                    if let Some(syntax) = evaluation(function.node, range) {
                        flow.statement(syntax, &mut effects);
                    }
                }
            }
            effects
        })
        .collect()
}

fn evaluation<'tree>(function: Node<'tree>, evaluation: &Evaluation) -> Option<Node<'tree>> {
    let inner = function.named_descendant_for_byte_range(evaluation.span.start, evaluation.span.end)?;
    // A wrapper and its only child can have identical spans.
    std::iter::successors(Some(inner), Node::parent)
        .take_while(|node| *node != function && node.byte_range() == evaluation.span)
        .find(|node| node.kind_id() == evaluation.syntax_kind)
}

struct Lower<'plan, 'spec, 'src> {
    plan: &'plan Plan<'spec, 'src>,
    source: &'src [u8],
    language: ContractLanguage,
    captured: HashSet<&'src str>,
}

impl Lower<'_, '_, '_> {
    fn statement(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        match node.kind() {
            "expression_statement" => {
                if let Some(expr) = site::children(node).next() {
                    if !site::identifier(expr) {
                        self.statement(expr, effects);
                    }
                }
            }
            "lexical_declaration"
            | "variable_declaration"
            | "local_variable_declaration"
            | "var_declaration"
            | "var_spec_list" => {
                for child in site::children(node) {
                    self.statement(child, effects);
                }
            }
            _ => match assignment_parts(node, self.source) {
                Some((target, value, reassignment)) => self.assignment(node, target, value, reassignment, effects),
                None => self.expression(node, effects),
            },
        }
    }

    fn assignment(
        &self,
        node: Node<'_>,
        target: Node<'_>,
        value: Option<Node<'_>>,
        reassignment: bool,
        effects: &mut Vec<Effect>,
    ) {
        if value.is_none()
            && self.language == ContractLanguage::Web
            && node
                .parent()
                .is_some_and(|parent| node.kind() == "variable_declarator" && parent.kind() == "variable_declaration")
        {
            return;
        }
        let targets = assignment_targets(node, target);
        let values = value.map_or_else(Vec::new, assignment_items);
        // A tuple-valued expression can be consumed, but its component
        // identities are outside this single-result contract.
        let paired = targets.len() == values.len();
        if !paired {
            for value in &values {
                self.reads(*value, effects);
            }
        }
        let writes = targets
            .into_iter()
            .enumerate()
            .filter_map(|(index, target)| {
                let value = paired.then(|| values[index]);
                self.write(target, value, reassignment, effects)
            })
            .collect();
        effects.push(Effect::Write(writes));
    }

    fn write(
        &self,
        target: Node<'_>,
        value: Option<Node<'_>>,
        reassignment: bool,
        effects: &mut Vec<Effect>,
    ) -> Option<Write> {
        let name = target.utf8_text(self.source).ok()?;
        let wildcard = name == "_" && self.language != ContractLanguage::Web;
        let binding = self.local(name, target.start_byte(), !reassignment).filter(|_| site::identifier(target));
        if !wildcard && binding.is_none() {
            if reassignment {
                self.reads(target, effects);
            }
            if let Some(value) = value {
                self.reads(value, effects);
            }
            return None;
        }
        Some(Write {
            target: if wildcard { None } else { binding },
            value: value.map_or(Value::Other, |node| self.value(node, effects)),
        })
    }

    fn value(&self, node: Node<'_>, effects: &mut Vec<Effect>) -> Value {
        let value = transparent(node);
        if site::identifier(value) {
            if let Some(binding) =
                value.utf8_text(self.source).ok().and_then(|name| self.local(name, value.start_byte(), false))
            {
                return Value::Copy(binding);
            }
        }
        if let Some(origin) = self.origin(value) {
            // Any arguments still read their previous values.
            self.reads(value, effects);
            return Value::Source(origin);
        }
        self.reads(node, effects);
        Value::Other
    }

    fn local(&self, name: &str, at: usize, defining: bool) -> Option<usize> {
        if self.captured.contains(name) {
            return None;
        }
        self.plan.syntax.local_result_binding(name, at, defining)
    }

    fn expression(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        let call = transparent(node);
        if site::identifier(call) && node.parent().is_some_and(|parent| parent.kind() == "expression_statement") {
            return;
        }
        self.reads(node, effects);
        let call = if node.kind() == "defer_statement" {
            node.named_child(0).unwrap_or(node)
        } else if site::capture(call, self.source).is_some_and(|site| site.discarded) {
            call
        } else {
            return;
        };
        if let Some(origin) = self.origin(call) {
            effects.push(Effect::Write(vec![Write { target: None, value: Value::Source(origin) }]));
        }
    }

    fn origin(&self, node: Node<'_>) -> Option<Origin> {
        if !matches!(node.kind(), "method_invocation" | "call_expression" | "new_expression") {
            return None;
        }
        let call = site::name(node, self.source)?;
        self.plan.source_mode(&call)?;
        Some(Origin { call, start: node.start_byte(), line: node.start_position().row as u32 + 1 })
    }

    fn reads(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        walk_names(node, self.source, self.language, |name, start| {
            if let Some(binding) = self.local(name, start, false) {
                effects.push(Effect::Read(binding));
            }
        });
    }
}

fn assignment_targets<'tree>(node: Node<'tree>, target: Node<'tree>) -> Vec<Node<'tree>> {
    if node.kind() == "var_spec" {
        return node.children_by_field_name("name", &mut node.walk()).collect();
    }
    assignment_items(target)
}

fn assignment_items(node: Node<'_>) -> Vec<Node<'_>> {
    if node.kind() == "expression_list" {
        return site::children(node).collect();
    }
    vec![node]
}

fn transparent(mut node: Node<'_>) -> Node<'_> {
    while matches!(
        node.kind(),
        "parenthesized_expression" | "as_expression" | "satisfies_expression" | "non_null_expression"
    ) {
        let Some(inner) = site::children(node).next() else { break };
        node = inner;
    }
    node
}
