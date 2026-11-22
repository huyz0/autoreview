use std::collections::HashSet;

use tree_sitter::Node;

use crate::cfg::Evaluation;
use crate::lower::{keyed_syntax::assignment_parts, site};

use super::super::{plan::Plan, ContractLanguage, KeyedFunction};

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

fn captured_names<'src>(function: Node<'_>, source: &'src [u8], language: ContractLanguage) -> HashSet<&'src str> {
    let mut captured = HashSet::new();
    let mut pending: Vec<_> = site::children(function).collect();
    while let Some(node) = pending.pop() {
        if capture_scope(node) {
            walk_names(node, source, language, |name, _| {
                captured.insert(name);
            });
        } else {
            pending.extend(site::children(node));
        }
    }
    captured
}

fn capture_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "func_literal"
            | "function_expression"
            | "function_declaration"
            | "generator_function"
            | "generator_function_declaration"
            | "arrow_function"
            | "method_definition"
            | "lambda_expression"
            | "lambda_literal"
            | "anonymous_function"
            | "closure_expression"
            | "async_block"
            | "class_declaration"
            | "class_body"
            | "object_literal"
    )
}

fn walk_names<'src>(
    node: Node<'_>,
    source: &'src [u8],
    language: ContractLanguage,
    mut visit: impl FnMut(&'src str, usize),
) {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if reference(node, source) {
            if let Ok(name) = node.utf8_text(source) {
                visit(name, node.start_byte());
            }
        }
        string_references(node, source, language, |name| visit(name, node.start_byte()));
        pending.extend(site::children(node));
    }
}

fn string_references<'src>(
    node: Node<'_>,
    source: &'src [u8],
    language: ContractLanguage,
    mut visit: impl FnMut(&'src str),
) {
    if node.kind() != "string_content" {
        return;
    }
    let Ok(text) = node.utf8_text(source) else { return };
    match language {
        // The Kotlin grammar emits $ and the short template name as
        // separate string_content nodes. ${...} has its own expression.
        ContractLanguage::Kotlin if text == "$" => {
            if let Some(part) = node.next_named_sibling()
                .filter(|sibling| sibling.kind() == "string_content" && sibling.start_byte() == node.end_byte())
                .and_then(|sibling| sibling.utf8_text(source).ok()).and_then(identifier_prefix)
            {
                visit(part);
            }
        }
        ContractLanguage::Rust
            if node
                .parent()
                .and_then(|literal| literal.parent())
                .is_some_and(|parent| parent.kind() == "token_tree") =>
        {
            for part in text.split('{').skip(1).filter_map(identifier_prefix) {
                visit(part);
            }
        }
        _ => {}
    }
}

fn identifier_prefix(text: &str) -> Option<&str> {
    let end = text.find(|c: char| c != '_' && !c.is_alphanumeric()).unwrap_or(text.len());
    (end > 0).then(|| &text[..end])
}

fn reference(node: Node<'_>, source: &[u8]) -> bool {
    if matches!(node.kind(), "shorthand_property_identifier" | "shorthand_field_identifier") {
        return true;
    }
    if !site::identifier(node) {
        return false;
    }
    let Some(parent) = node.parent() else { return true };
    match parent.kind() {
        "navigation_expression" => parent.named_child(0) == Some(node),
        "macro_invocation" => parent.child_by_field_name("macro") != Some(node),
        "method_invocation" | "field_access" | "variable_declarator" => {
            parent.child_by_field_name("name") != Some(node)
        }
        "variable_declaration" | "scoped_identifier" | "jsx_attribute" | "jsx_closing_element" => false,
        "jsx_opening_element" | "jsx_self_closing_element" => {
            node.utf8_text(source).is_ok_and(|name| !name.as_bytes().first().is_some_and(u8::is_ascii_lowercase))
        }
        _ => true,
    }
}
