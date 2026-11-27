use tree_sitter::Node;

use crate::lower::site;

use super::super::references::{reference, string_references};
use super::super::syntax::transparent;
use super::{Demand, Effect, HelperCall, Lower, Place, Value, Write};

enum Visit<'tree> {
    Syntax(Node<'tree>, Demand),
    Invoke(usize, Demand),
}

fn specialize(outcome: &super::super::HelperOutcome, arguments: &[Node<'_>], source: &[u8], kotlin: bool, syntax: &crate::call_order::identity::Syntax<'_>) -> (Vec<bool>, Vec<Option<bool>>) {
    let mut used = outcome.parameters.clone();
    let mut unobserved = outcome.unobserved_parameters.clone();
    let Some(conditional) = &outcome.conditional_parameters else { return (used, unobserved); };
    let Some(argument) = arguments.get(conditional.guard_index).copied().map(transparent) else { return (used, unobserved); };
    let values = match boolean_value(argument, source, kotlin, syntax) {
        Some(true) => &conditional.when_true,
        Some(false) => &conditional.when_false,
        _ => return (used, unobserved),
    };
    for (index, value) in values.iter().enumerate() {
        if let Some(can_skip) = value {
            used[index] = !can_skip;
            unobserved[index] = Some(*can_skip);
        }
    }
    (used, unobserved)
}

fn boolean_value(argument: Node<'_>, source: &[u8], kotlin: bool, syntax: &crate::call_order::identity::Syntax<'_>) -> Option<bool> {
    let kind = if (kotlin && argument.kind() == "identifier") || argument.kind() == "boolean_literal" { argument.utf8_text(source).ok()? } else { argument.kind() };
    match kind {
        "true" => Some(true),
        "false" => Some(false),
        _ if argument.kind() == "identifier" => argument.utf8_text(source).ok().and_then(|name| syntax.stable_boolean(name, argument.start_byte())),
        _ => None,
    }
}

impl Lower<'_, '_, '_> {
    fn helper_guard(&self, summary: &super::HelperSummary, arguments: &[Node<'_>]) -> Option<crate::cfg::IdentityOperand> {
        let conditional = summary.when_observed.conditional_parameters.as_ref()?;
        let argument = transparent(*arguments.get(conditional.guard_index)?);
        if argument.kind() != "identifier" { return None; }
        self.plan.syntax.stable_identity(argument.utf8_text(self.source).ok()?, argument.start_byte())
    }

    pub(super) fn helper_call<'tree>(&self, node: Node<'tree>) -> Option<(Vec<Node<'tree>>, &super::HelperSummary)> {
        // Helper summaries still use start-only identities. Do not apply one
        // to either of two calls when a verified source shares that start.
        if self.plan.extra_spans.range((node.start_byte(), 0)..=(node.start_byte(), usize::MAX)).next().is_some() { return None; }
        let summary = self.helpers.get(&node.start_byte())?;
        let arguments = site::arguments(node)?;
        let arguments: Option<Vec<_>> = site::children(arguments).map(site::argument).collect();
        let arguments = arguments?;
        let arity = if arguments.len() == 1 {
            self.plan.result_components.get(&transparent(arguments[0]).start_byte())
                .map_or(1, |shape| shape.count)
        } else { arguments.len() };
        (arity == summary.when_discarded.parameters.len()).then_some((arguments, summary))
    }

    pub(super) fn closure_call(&self, node: Node<'_>) -> Option<usize> {
        self.inside.is_none().then(|| self.closures.call(node)).flatten()
    }

    pub(super) fn reads(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        self.reads_with(node, effects, Demand::Observed);
    }

    pub(super) fn reads_with(&self, node: Node<'_>, effects: &mut Vec<Effect>, demand: Demand) {
        let mut todo = vec![Visit::Syntax(node, demand)];
        while let Some(visit) = todo.pop() {
            match visit {
                Visit::Syntax(node, demand) => self.visit(node, demand, &mut todo, effects),
                Visit::Invoke(index, demand) => effects.push(Effect::Invoke(index, demand)),
            }
        }
    }

    fn visit<'tree>(&self, node: Node<'tree>, demand: Demand, todo: &mut Vec<Visit<'tree>>, effects: &mut Vec<Effect>) {
        self.opaque_wrapped_arguments(node, effects);
        if self.add_helper_effect(node, demand, effects) { return; }
        if let Some(closure) = self.closures.literal(node) {
            let snapshots = closure
                .snapshots
                .iter()
                .map(|&(source, target)| Write { target: Some(target), value: Value::Copy(source) })
                .collect();
            effects.push(Effect::Write(snapshots));
            return;
        }
        self.opaque_arguments(node, effects);
        if let Some(place) = self.places.place(node, false).filter(|place| matches!(place, Place::Field(..))) {
            effects.push(Effect::Read(self.remap(place)));
            return;
        }
        self.read_names(node, effects);
        if let Some(index) = self.closure_call(node) {
            todo.push(Visit::Invoke(index, demand));
        }
        let parts: Vec<_> = site::children(node).collect();
        todo.extend(parts.into_iter().rev().map(|child| Visit::Syntax(child, Demand::Observed)));
    }

    fn add_helper_effect(&self, node: Node<'_>, demand: Demand, effects: &mut Vec<Effect>) -> bool {
        let Some((arguments, summary)) = self.helper_call(node) else { return false; };
        let language = self.plan.spec.contract.as_ref().map(|contract| contract.language);
        let kotlin = language == Some(crate::call_order::ContractLanguage::Kotlin);
        // Go permits shadowing its predeclared boolean names, including where
        // the syntax tree gives them literal-shaped nodes.
        let shadowed = language == Some(crate::call_order::ContractLanguage::Go);
        let guard = (!shadowed).then(|| self.helper_guard(summary, &arguments)).flatten();
        let (discarded, observed) = if shadowed {
            ((summary.when_discarded.parameters.clone(), summary.when_discarded.unobserved_parameters.clone()),
             (summary.when_observed.parameters.clone(), summary.when_observed.unobserved_parameters.clone()))
        } else {
            (specialize(&summary.when_discarded, &arguments, self.source, kotlin, &self.plan.syntax),
             specialize(&summary.when_observed, &arguments, self.source, kotlin, &self.plan.syntax))
        };
        let Some(arguments) = self.lower_helper_arguments(arguments, effects) else { return true; };
        let origins = |values: &[(super::Origin, bool)]| {
            values
                .iter()
                .cloned()
                .map(|(mut origin, used)| {
                    let call = (self.plan.syntax.unit, node.start_byte());
                    if !origin.calls.contains(&call) {
                        origin.calls.push(call);
                        origin.calls.sort_unstable();
                    }
                    (origin, used)
                })
                .collect()
        };
        let opaque_origins = |values: &[super::Origin]| {
            values.iter().cloned().map(|mut origin| {
                let call = (self.plan.syntax.unit, node.start_byte());
                if !origin.calls.contains(&call) { origin.calls.push(call); origin.calls.sort_unstable(); }
                origin
            }).collect()
        };
        effects.push(Effect::ApplyHelper(Box::new(HelperCall {
            arguments,
            demand,
            when_discarded: discarded.0,
            when_observed: observed.0,
            unobserved_discarded: discarded.1,
            unobserved_observed: observed.1,
            guard,
            conditional_discarded: summary.when_discarded.conditional_parameters.clone(),
            conditional_observed: summary.when_observed.conditional_parameters.clone(),
            origins_discarded: origins(&summary.when_discarded.origins),
            origins_observed: origins(&summary.when_observed.origins),
            opaque_discarded: summary.when_discarded.opaque_parameters.clone(),
            opaque_observed: summary.when_observed.opaque_parameters.clone(),
            opaque_origins_discarded: opaque_origins(&summary.when_discarded.opaque_origins),
            opaque_origins_observed: opaque_origins(&summary.when_observed.opaque_origins),
        })));
        true
    }

    fn lower_helper_arguments(&self, arguments: Vec<Node<'_>>, effects: &mut Vec<Effect>) -> Option<Vec<Option<Place>>> {
        let multi = (arguments.len() == 1).then(|| transparent(arguments[0]))
            .and_then(|call| self.plan.result_components.get(&call.start_byte()).map(|shape| (call, shape)));
        let arguments = if let Some((call, shape)) = multi {
            let origin = self.origin(call)?;
            self.reads(call, effects);
            (0..shape.count).map(|index| {
                let place = Place::CallComponent(call.start_byte(), index);
                let value = if shape.indices.contains(&index) {
                    let mut component = origin.clone();
                    component.component = Some(index);
                    Value::Source(component)
                } else { Value::Other };
                effects.push(Effect::Write(vec![Write { target: Some(place), value }]));
                Some(place)
            }).collect()
        } else { arguments
            .into_iter()
            .map(|argument| {
                if let Some(place) = self.place(argument, false) {
                    return Some(place);
                }
                let temporary = Place::CallResult(argument.start_byte(), argument.end_byte());
                let value = self.value(argument, effects);
                effects.push(Effect::Write(vec![Write { target: Some(temporary), value }]));
                Some(temporary)
            })
            .collect() };
        Some(arguments)
    }

    fn opaque_arguments(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        if !matches!(node.kind(), "call_expression" | "method_invocation" | "new_expression") || self.origin(node).is_some() { return; }
        let Some(arguments) = site::arguments(node) else { return; };
        for argument in site::children(arguments).filter_map(site::argument) {
            let argument = transparent(argument);
            if let Some(place) = self.place(argument, false) {
                effects.push(Effect::OpaqueRead(place));
            } else if self.helper_call(argument).is_some() {
                let temporary = Place::CallResult(argument.start_byte(), argument.end_byte());
                let mut nested = Vec::new();
                self.reads_with(argument, &mut nested, Demand::Slot(temporary));
                effects.push(Effect::OpaqueOnly(nested));
                effects.push(Effect::OpaqueRead(temporary));
            }
        }
    }

    fn opaque_wrapped_arguments(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        if !matches!(node.kind(), "call_expression" | "method_invocation" | "new_expression") || self.origin(node).is_some() { return; }
        let Some(arguments) = site::arguments(node) else { return; };
        for argument in site::children(arguments).filter_map(site::argument) {
            let argument = transparent(argument);
            if !matches!(argument.kind(), "array" | "spread_element" | "spread_expression") { continue; }
            let mut pending = vec![argument];
            while let Some(current) = pending.pop() {
                let current = transparent(current);
                if let Some(place) = self.place(current, false) {
                    effects.push(Effect::OpaqueRead(place));
                } else if matches!(current.kind(), "array" | "spread_element" | "spread_expression") {
                    pending.extend(site::children(current));
                }
            }
        }
    }

    fn read_names(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        let mut read = |name: &str| {
            if let Some(binding) = self.places.named(name, node.start_byte(), false) {
                effects.push(Effect::Read(self.remap(Place::Local(binding))));
            }
        };
        if reference(node, self.source) {
            if let Ok(name) = node.utf8_text(self.source) {
                read(name);
            }
        }
        string_references(node, self.source, self.language, read);
    }
}
