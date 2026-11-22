use std::collections::{HashMap, HashSet};

use autoreview_langsupport::Language;
use tree_sitter::Node;

use crate::cfg::{Cfg, Stmt};
use crate::lower::{self, keyed_syntax::assignment_parts, site};

use super::places::{Place, Places};
use super::references::{capture_scope, walk_names};
use super::syntax::{descendants, initializers, items, targets, transparent};
use super::ContractLanguage;

pub(super) struct Closures<'tree> {
    pub(super) bodies: Vec<Closure<'tree>>,
    literals: HashMap<usize, usize>,
    calls: HashMap<(usize, usize), usize>,
}

pub(super) struct Closure<'tree> {
    pub(super) node: Node<'tree>,
    pub(super) graph: Cfg<Stmt>,
    pub(super) snapshots: Vec<(Place, Place)>,
    pub(super) tail: Option<Node<'tree>>,
}

struct Candidate<'tree> {
    literal: Node<'tree>,
    target: Option<usize>,
    calls: Vec<(usize, usize)>,
    uncertain: bool,
}

impl<'tree> Closures<'tree> {
    pub(super) fn scan(
        function: Node<'tree>,
        language: ContractLanguage,
        places: &Places<'_, '_>,
        source: &[u8],
    ) -> Self {
        let mut candidates = candidates(function, places, source);
        let bindings = candidates.iter().enumerate().filter_map(|(id, item)| Some((item.target?, id))).collect();
        resolve_calls(function, &bindings, places, language, &mut candidates);
        let mut closures = Self { bodies: Vec::new(), literals: HashMap::new(), calls: HashMap::new() };
        for candidate in candidates.into_iter().filter(|item| !item.uncertain) {
            if let Some(body) = Closure::compile(candidate.literal, function, language, places, source) {
                let id = closures.bodies.len();
                closures.literals.insert(candidate.literal.start_byte(), id);
                closures.calls.extend(candidate.calls.into_iter().map(|start| (start, id)));
                closures.bodies.push(body);
            }
        }
        closures
    }

    pub(super) fn literal(&self, node: Node<'_>) -> Option<&Closure<'tree>> {
        self.literals.get(&node.start_byte()).and_then(|&id| {
            let body = &self.bodies[id];
            (body.node.byte_range() == node.byte_range() && body.node.kind() == node.kind()).then_some(body)
        })
    }

    pub(super) fn call(&self, node: Node<'_>) -> Option<usize> {
        matches!(node.kind(), "call_expression" | "method_invocation")
            .then(|| self.calls.get(&(node.start_byte(), node.end_byte())).copied())
            .flatten()
    }
}

fn candidates<'tree>(function: Node<'tree>, places: &Places<'_, '_>, source: &[u8]) -> Vec<Candidate<'tree>> {
    let mut assigned = HashMap::new();
    for (target, value) in descendants(function).flat_map(|node| initializers(node, source)) {
        if literal(value) {
            if let Some(binding) = places.binding(target, true).filter(|&binding| binding == target.start_byte()) {
                assigned.insert(value.start_byte(), binding);
            }
        }
    }
    descendants(function)
        .filter(|node| literal(*node) && in_owner(*node, function))
        .filter_map(|node| {
            let target = assigned.get(&node.start_byte()).copied();
            let immediate = callee_use(node, ContractLanguage::Web, source);
            let mut calls = Vec::new();
            if let Some(call) = immediate.filter(|call| ordinary_call(node, *call)) {
                calls.push((call.start_byte(), call.end_byte()));
            }
            (target.is_some() || !calls.is_empty()).then_some(Candidate {
                literal: node,
                target,
                calls,
                uncertain: false,
            })
        })
        .collect()
}

fn resolve_calls(
    function: Node<'_>,
    bindings: &HashMap<usize, usize>,
    places: &Places<'_, '_>,
    language: ContractLanguage,
    candidates: &mut [Candidate<'_>],
) {
    for reference in descendants(function).filter(|node| site::identifier(*node)) {
        let Some(binding) = places.binding(reference, true).filter(|&binding| binding != reference.start_byte()) else {
            continue;
        };
        let Some(&candidate) = bindings.get(&binding) else { continue };
        if let Some(call) = callee_use(reference, language, places.source())
            .filter(|call| ordinary_call(candidates[candidate].literal, *call) && in_owner(*call, function))
        {
            candidates[candidate].calls.push((call.start_byte(), call.end_byte()));
        } else if !discarded_reference(reference, language, places.source()) {
            candidates[candidate].uncertain = true;
        }
    }
}

fn discarded_reference(reference: Node<'_>, language: ContractLanguage, source: &[u8]) -> bool {
    if !matches!(language, ContractLanguage::Go | ContractLanguage::Rust) {
        return false;
    }
    let assignment = std::iter::successors(reference.parent(), Node::parent)
        .find(|node| !matches!(node.kind(), "expression_list" | "parenthesized_expression"));
    assignment.and_then(|node| assignment_parts(node, source)).is_some_and(|(target, value, _)| {
        target.utf8_text(source) == Ok("_")
            && value.is_some_and(|value| items(value).as_slice() == [reference] || transparent(value) == reference)
    })
}

impl<'tree> Closure<'tree> {
    fn compile(
        node: Node<'tree>,
        owner: Node<'_>,
        language: ContractLanguage,
        places: &Places<'_, '_>,
        source: &[u8],
    ) -> Option<Self> {
        if descendants(node).skip(1).any(capture_scope) {
            return None;
        }
        let captures = captures(node, language, places, source)?;
        let moving =
            language == ContractLanguage::Rust && node.children(&mut node.walk()).any(|token| token.kind() == "move");
        if language == ContractLanguage::Rust && !moving && outer_rebinds_capture(owner, node, places, &captures) {
            return None;
        }
        let graph = lower::result_closure(grammar(language), source, node)?;
        if graph.keyed_flow == Some(0) || graph.edges.is_empty() {
            return None;
        }
        let snapshots = if moving {
            captures
                .into_iter()
                .enumerate()
                .map(|(id, capture)| (capture, Place::Capture(node.start_byte(), id)))
                .collect()
        } else {
            Vec::new()
        };
        Some(Self { node, graph, snapshots, tail: tail(node, language) })
    }

    pub(super) fn remap(&self, place: Place) -> Place {
        self.snapshots.iter().find_map(|&(original, snapshot)| (place == original).then_some(snapshot)).unwrap_or(place)
    }
}

fn captures(node: Node<'_>, language: ContractLanguage, places: &Places<'_, '_>, source: &[u8]) -> Option<Vec<Place>> {
    let mut captured = Vec::new();
    let mut unknown = false;
    walk_names(node, source, language, |name, at| {
        if let Some(binding) = places.named(name, at, false) {
            if !node.byte_range().contains(&binding) {
                captured.push(Place::Local(binding));
            }
        } else if places.has_binding(name) && places.named(name, at, true).is_none() {
            unknown = true;
        }
    });
    for child in descendants(node) {
        if let Some(place @ Place::Field(binding, _)) = places.place(child, false) {
            if !node.byte_range().contains(&binding) {
                captured.push(place);
            }
        }
    }
    let mut seen = HashSet::new();
    captured.retain(|place| seen.insert(*place));
    (!unknown).then_some(captured)
}

fn outer_rebinds_capture(owner: Node<'_>, closure: Node<'_>, places: &Places<'_, '_>, captures: &[Place]) -> bool {
    descendants(owner).filter(|node| node.start_byte() > closure.end_byte()).any(|node| {
        let Some((target, _, true)) = assignment_parts(node, places.source()) else { return false };
        targets(node, target)
            .into_iter()
            .any(|target| places.place(target, false).is_some_and(|place| captures.contains(&place)))
    })
}

fn callee_use<'tree>(node: Node<'tree>, language: ContractLanguage, source: &[u8]) -> Option<Node<'tree>> {
    let expr =
        std::iter::successors(Some(node), Node::parent).take_while(|parent| transparent(*parent) == node).last()?;
    let call = expr.parent()?;
    match call.kind() {
        "call_expression" => {
            let target = call.child_by_field_name("function").or_else(|| call.named_child(0));
            (target == Some(expr) && !call.children(&mut call.walk()).any(|token| token.kind() == "optional_chain"))
                .then_some(call)
        }
        "method_invocation" if language == ContractLanguage::Java => java_sam(call, expr, source).then_some(call),
        _ => None,
    }
}

fn java_sam(call: Node<'_>, object: Node<'_>, source: &[u8]) -> bool {
    call.child_by_field_name("object") == Some(object)
        && call.child_by_field_name("name").and_then(|name| name.utf8_text(source).ok()).is_some_and(|name| {
            matches!(
                name,
                "run"
                    | "call"
                    | "get"
                    | "apply"
                    | "accept"
                    | "test"
                    | "compare"
                    | "getAsBoolean"
                    | "getAsInt"
                    | "getAsLong"
                    | "getAsDouble"
            )
        })
}

fn ordinary_call(closure: Node<'_>, call: Node<'_>) -> bool {
    // The local invocation summary follows normal return. A caller's
    // catch can observe state left by a throwing callback.
    let protected = std::iter::successors(call.parent(), Node::parent)
        .take_while(|ancestor| !capture_scope(*ancestor) && ancestor.kind() != "method_declaration")
        .any(|ancestor| {
            ancestor.kind() == "try_statement"
                || ancestor.kind() == "try_expression" && ancestor.child(0).is_some_and(|token| token.kind() == "try")
        });
    if protected {
        return false;
    }
    let context = std::iter::successors(call.parent(), Node::parent).find(|ancestor| transparent(*ancestor) != call);
    if closure.children(&mut closure.walk()).any(|token| token.kind() == "async") {
        return context.is_some_and(|parent| parent.kind() == "await_expression");
    }
    !context.is_some_and(|parent| matches!(parent.kind(), "defer_statement" | "go_statement"))
}

fn in_owner(node: Node<'_>, owner: Node<'_>) -> bool {
    std::iter::successors(node.parent(), Node::parent).find(|parent| *parent == owner || capture_scope(*parent))
        == Some(owner)
}

fn literal(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "func_literal"
            | "lambda_expression"
            | "lambda_literal"
            | "closure_expression"
            | "arrow_function"
            | "function_expression"
    ) && !node.children(&mut node.walk()).any(|token| token.kind() == "*")
}

fn tail(node: Node<'_>, language: ContractLanguage) -> Option<Node<'_>> {
    if node.kind() == "lambda_literal" {
        return site::children(node).last();
    }
    let body = node.child_by_field_name("body")?;
    match body.kind() {
        "block" if language == ContractLanguage::Rust => site::children(body)
            .last()
            .filter(|last| !matches!(last.kind(), "expression_statement" | "let_declaration")),
        "block" | "statement_block" => None,
        _ => Some(body),
    }
}

fn grammar(language: ContractLanguage) -> Language {
    match language {
        ContractLanguage::Go => Language::Go,
        ContractLanguage::Java => Language::Java,
        ContractLanguage::Kotlin => Language::Kotlin,
        ContractLanguage::Rust => Language::Rust,
        ContractLanguage::Web => Language::JavaScript,
    }
}
