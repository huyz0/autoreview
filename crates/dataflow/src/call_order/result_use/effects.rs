use std::collections::BTreeMap;
use tree_sitter::Node;

use crate::cfg::{Cfg, Evaluation, Stmt};
use crate::lower::{keyed_syntax::assignment_parts, site};

use super::super::KeyedFunction;
use super::closures::{Closure, Closures};
use super::places::{Place, Places};
use super::references::captured_names;
use super::syntax::{items as assignment_items, targets as assignment_targets, transparent};
use super::{Analysis, Body, ContractLanguage, Flow, HelperSummary, Plan};

mod reads;

mod model;
pub(super) use model::{Demand, Effect, HelperCall, Origin, Value, Write};

pub(super) fn lower<'src>(
    plan: &Plan<'_, 'src>,
    function: &KeyedFunction<'_, '_>,
    source: &'src [u8],
    returns_to_caller: bool,
    helpers: &BTreeMap<usize, HelperSummary>,
) -> Analysis {
    let language = plan.spec.contract.as_ref().unwrap().language;
    let mut places = Places::new(plan, function.node, source);
    let closures = Closures::scan(function.node, language, &places, source);
    places.exclude_captures(captured_names(function.node, source, language, |node| {
        closures.literal(node).is_some()
    }));
    let return_tail = returns_to_caller
        .then(|| function_return_tail(function.node, language))
        .flatten();
    let lower = Lower {
        plan,
        source,
        language,
        places: &places,
        closures: &closures,
        inside: None,
        returns_to_caller,
        return_tail,
        helpers,
    };
    let owner = lower.body(function.node, function.graph);
    let bodies = closures
        .bodies
        .iter()
        .map(|closure| {
            Lower {
                inside: Some(closure),
                ..lower
            }
            .body(closure.node, &closure.graph)
        })
        .collect();
    Analysis {
        owner,
        closures: bodies,
    }
}

fn evaluation<'tree>(function: Node<'tree>, evaluation: &Evaluation) -> Option<Node<'tree>> {
    let inner =
        function.named_descendant_for_byte_range(evaluation.span.start, evaluation.span.end)?;
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
    returns_to_caller: bool,
    return_tail: Option<Node<'tree>>,
    helpers: &'plan BTreeMap<usize, HelperSummary>,
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
                    for syntax in node
                        .evaluations
                        .iter()
                        .filter_map(|range| evaluation(function, range))
                    {
                        self.statement(syntax, &mut effects);
                    }
                }
                effects
            })
            .collect();
        Body { flow, effects }
    }

    fn statement(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        let value = if let Some(closure) = self.inside {
            closure_result(node, closure)
        } else if self.returns_to_caller {
            return_value(node).or_else(|| {
                self.return_tail
                    .filter(|tail| {
                        tail.byte_range() == node.byte_range() && tail.kind() == node.kind()
                    })
                    .map(|_| node)
            })
        } else {
            None
        };
        if let Some(value) = value {
            let value = self.value(value, effects);
            effects.push(Effect::Write(vec![Write {
                target: Some(Place::Return),
                value,
            }]));
            return;
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
                Some((target, value, reassignment)) => {
                    self.assignment(node, target, value, reassignment, effects)
                }
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
            && node.parent().is_some_and(|parent| {
                node.kind() == "variable_declarator" && parent.kind() == "variable_declaration"
            })
        {
            return;
        }
        let targets = assignment_targets(node, target);
        let values = value.map_or_else(Vec::new, assignment_items);
        if let Some(call) = (values.len() == 1).then(|| transparent(values[0])) {
            if self.component_assignment(&targets, call, reassignment, effects) { return; }
        }
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
            let initialized = writes
                .iter()
                .flat_map(|write| self.allocation_fields(write.target))
                .collect::<Vec<_>>();
            writes.extend(initialized);
        }
        effects.push(Effect::Write(writes));
    }

    fn component_assignment(&self, targets: &[Node<'_>], call: Node<'_>, reassignment: bool, effects: &mut Vec<Effect>) -> bool {
        let Some(shape) = self.plan.result_components.get(&call.start_byte()) else { return false; };
        if shape.count != targets.len() { return false; }
        let Some(origin) = self.origin(call) else { return false; };
        self.reads(call, effects);
        let writes = targets.iter().enumerate().filter_map(|(index, target)| {
            let mut write = self.write(*target, None, reassignment, effects)?;
            if shape.indices.contains(&index) {
                let mut component = origin.clone();
                component.component = Some(index);
                write.value = Value::Source(component);
            }
            Some(write)
        }).collect();
        effects.push(Effect::Write(writes));
        true
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
        if self.language == ContractLanguage::Web && value.kind() == "array" {
            return Value::Aggregate(self.array_members(value, effects));
        }
        if let Some(binding) = self.place(value, false) {
            return Value::Copy(binding);
        }
        if self.closure_call(value).is_some() || self.helper_call(value).is_some() {
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

    fn array_members(&self, node: Node<'_>, effects: &mut Vec<Effect>) -> Vec<Value> {
        site::children(node).map(|item| {
            let member = self.value(item, effects);
            if let Value::Copy(place) = member {
                effects.push(Effect::ObservedOnly(vec![Effect::Read(place)]));
                Value::Copy(place)
            } else { member }
        }).collect()
    }

    fn place(&self, node: Node<'_>, defining: bool) -> Option<Place> {
        self.places
            .place(node, defining)
            .map(|place| self.remap(place))
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
            .map(|target| Write {
                target: Some(target),
                value: Value::Other,
            })
    }

    fn expression(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        let call = transparent(node);
        if self.place(call, false).is_some()
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "expression_statement")
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
            effects.push(Effect::Write(vec![Write {
                target: None,
                value: Value::Source(origin),
            }]));
        }
    }

    fn origin(&self, node: Node<'_>) -> Option<Origin> {
        if !matches!(
            node.kind(),
            "method_invocation" | "call_expression" | "new_expression"
        ) {
            return None;
        }
        let call = site::name(node, self.source)?;
        self.plan.source_mode_at(&call, node.start_byte())?;
        Some(Origin {
            call,
            start: node.start_byte(),
            component: None,
            line: node.start_position().row as u32 + 1,
            path: None,
            calls: Vec::new(),
        })
    }
}

fn closure_result<'tree>(node: Node<'tree>, closure: &Closure<'_>) -> Option<Node<'tree>> {
    if let Some(value) = return_value(node) {
        return Some(value);
    }
    if closure
        .tail
        .is_some_and(|tail| tail.byte_range() == node.byte_range() && tail.kind() == node.kind())
        && !matches!(
            node.kind(),
            "assignment" | "assignment_expression" | "property_declaration"
        )
    {
        return Some(node);
    }
    None
}

fn return_value(node: Node<'_>) -> Option<Node<'_>> {
    if !matches!(node.kind(), "return_statement" | "return_expression") {
        return None;
    }
    let values = assignment_items(site::children(node).next()?);
    (values.len() == 1).then(|| values[0])
}

fn function_return_tail(node: Node<'_>, language: ContractLanguage) -> Option<Node<'_>> {
    match language {
        ContractLanguage::Rust => {
            let body = node.child_by_field_name("body")?;
            site::children(body).last().filter(|tail| {
                !matches!(
                    tail.kind(),
                    "expression_statement"
                        | "let_declaration"
                        | "return_expression"
                        | "if_expression"
                        | "match_expression"
                )
            })
        }
        ContractLanguage::Kotlin => crate::lower::kotlin::expression_body(node),
        ContractLanguage::Web if node.kind() == "arrow_function" => node
            .child_by_field_name("body")
            .filter(|body| body.kind() != "statement_block"),
        _ => None,
    }
}
