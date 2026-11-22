use tree_sitter::Node;

use crate::cfg::{Cfg, Evaluation, Stmt};
use crate::lower::{keyed_syntax::assignment_parts, site};

use super::super::KeyedFunction;
use super::closures::{Closure, Closures};
use super::places::{Place, Places};
use super::references::captured_names;
use super::syntax::{items as assignment_items, targets as assignment_targets, transparent};
use super::{Analysis, Body, ContractLanguage, Flow, Live, Plan};

mod reads;

#[derive(Clone)]
pub(super) struct Origin {
    pub(super) call: String,
    pub(super) start: usize,
    pub(super) line: u32,
}

pub(super) enum Value {
    Source(Origin),
    Copy(Place),
    Other,
}

pub(super) struct Write {
    pub(super) target: Option<Place>,
    pub(super) value: Value,
}

pub(super) enum Effect {
    Read(Place),
    Write(Vec<Write>),
    Invoke(usize, Demand),
}

#[derive(Clone, Copy)]
pub(super) enum Demand {
    Discarded,
    Observed,
    Slot(Place),
}

impl Demand {
    pub(super) fn observed(self, live: &mut Live) -> bool {
        match self {
            Self::Discarded => false,
            Self::Observed => true,
            Self::Slot(slot) => live.remove(&slot),
        }
    }
}

pub(super) fn lower<'src>(plan: &Plan<'_, 'src>, function: &KeyedFunction<'_, '_>, source: &'src [u8]) -> Analysis {
    let language = plan.spec.contract.as_ref().unwrap().language;
    let mut places = Places::new(plan, function.node, source);
    let closures = Closures::scan(function.node, language, &places, source);
    places.exclude_captures(captured_names(function.node, source, language, |node| closures.literal(node).is_some()));
    let lower = Lower { plan, source, language, places: &places, closures: &closures, inside: None };
    let owner = lower.body(function.node, function.graph);
    let bodies = closures
        .bodies
        .iter()
        .map(|closure| Lower { inside: Some(closure), ..lower }.body(closure.node, &closure.graph))
        .collect();
    Analysis { owner, closures: bodies }
}

fn evaluation<'tree>(function: Node<'tree>, evaluation: &Evaluation) -> Option<Node<'tree>> {
    let inner = function.named_descendant_for_byte_range(evaluation.span.start, evaluation.span.end)?;
    // A wrapper and its only child can have identical spans.
    std::iter::successors(Some(inner), Node::parent)
        .take_while(|node| *node != function && node.byte_range() == evaluation.span)
        .find(|node| node.kind_id() == evaluation.syntax_kind)
}

struct Lower<'plan, 'src, 'tree> {
    plan: &'plan Plan<'plan, 'src>,
    source: &'src [u8],
    language: ContractLanguage,
    places: &'plan Places<'plan, 'src>,
    closures: &'plan Closures<'tree>,
    inside: Option<&'plan Closure<'tree>>,
}

impl Lower<'_, '_, '_> {
    fn body(&self, function: Node<'_>, graph: &Cfg<Stmt>) -> Body {
        let flow = Flow::new(graph, self.plan);
        let effects = graph
            .nodes
            .iter()
            .zip(&flow.reachable)
            .map(|(node, &reached)| {
                let mut effects = Vec::new();
                if reached {
                    for syntax in node.evaluations.iter().filter_map(|range| evaluation(function, range)) {
                        self.statement(syntax, &mut effects);
                    }
                }
                effects
            })
            .collect();
        Body { flow, effects }
    }

    fn statement(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        if let Some(closure) = self.inside {
            if let Some(value) = closure_result(node, closure) {
                let value = self.value(value, effects);
                effects.push(Effect::Write(vec![Write { target: Some(Place::Return), value }]));
                return;
            }
        }
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
        let mut writes: Vec<_> = targets
            .into_iter()
            .enumerate()
            .filter_map(|(index, target)| {
                let value = paired.then(|| values[index]);
                self.write(target, value, reassignment, effects)
            })
            .collect();
        if !reassignment {
            let initialized = writes.iter().flat_map(|write| self.allocation_fields(write.target)).collect::<Vec<_>>();
            writes.extend(initialized);
        }
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
        let binding = self.place(target, !reassignment);
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
        if let Some(binding) = self.place(value, false) {
            return Value::Copy(binding);
        }
        if self.closure_call(value).is_some() {
            let output = Place::CallResult(value.start_byte());
            self.reads_with(value, effects, Demand::Slot(output));
            return Value::Copy(output);
        }
        if let Some(origin) = self.origin(value) {
            // Any arguments still read their previous values.
            self.reads(value, effects);
            return Value::Source(origin);
        }
        self.reads(node, effects);
        Value::Other
    }

    fn place(&self, node: Node<'_>, defining: bool) -> Option<Place> {
        self.places.place(node, defining).map(|place| self.remap(place))
    }

    fn remap(&self, place: Place) -> Place {
        self.inside.map_or(place, |closure| closure.remap(place))
    }

    fn allocation_fields(&self, target: Option<Place>) -> impl Iterator<Item = Write> + '_ {
        target
            .into_iter()
            .filter_map(|place| match place {
                Place::Local(binding) => Some(binding),
                _ => None,
            })
            .flat_map(|binding| self.places.initialized_fields(binding))
            .map(|target| Write { target: Some(target), value: Value::Other })
    }

    fn expression(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        let call = transparent(node);
        if self.place(call, false).is_some()
            && node.parent().is_some_and(|parent| parent.kind() == "expression_statement")
        {
            return;
        }
        let discarded = site::capture(call, self.source).is_some_and(|site| site.discarded);
        if discarded {
            self.reads_with(call, effects, Demand::Discarded);
        } else {
            self.reads(node, effects);
        }
        let call = if node.kind() == "defer_statement" {
            node.named_child(0).unwrap_or(node)
        } else if discarded {
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
}

fn closure_result<'tree>(node: Node<'tree>, closure: &Closure<'_>) -> Option<Node<'tree>> {
    if matches!(node.kind(), "return_statement" | "return_expression") {
        let values = assignment_items(site::children(node).next()?);
        return (values.len() == 1).then(|| values[0]);
    }
    if closure.tail.is_some_and(|tail| tail.byte_range() == node.byte_range() && tail.kind() == node.kind())
        && !matches!(node.kind(), "assignment" | "assignment_expression" | "property_declaration")
    {
        return Some(node);
    }
    None
}
