use tree_sitter::Node;

use crate::lower::site;

use super::super::references::{reference, string_references};
use super::super::syntax::transparent;
use super::{Demand, Effect, HelperCall, Lower, Place, Value, Write};

enum Visit<'tree> {
    Syntax(Node<'tree>, Demand),
    Invoke(usize, Demand),
}

impl Lower<'_, '_, '_> {
    pub(super) fn helper_call<'tree>(&self, node: Node<'tree>) -> Option<(Vec<Node<'tree>>, &super::HelperSummary)> {
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
        let multi = (arguments.len() == 1).then(|| transparent(arguments[0]))
            .and_then(|call| self.plan.result_components.get(&call.start_byte()).map(|shape| (call, shape)));
        let arguments = if let Some((call, shape)) = multi {
            let Some(origin) = self.origin(call) else { return true; };
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
                let temporary = Place::CallResult(argument.start_byte());
                let value = self.value(argument, effects);
                effects.push(Effect::Write(vec![Write { target: Some(temporary), value }]));
                Some(temporary)
            })
            .collect() };
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
            when_discarded: summary.when_discarded.parameters.clone(),
            when_observed: summary.when_observed.parameters.clone(),
            unobserved_discarded: summary.when_discarded.unobserved_parameters.clone(),
            unobserved_observed: summary.when_observed.unobserved_parameters.clone(),
            origins_discarded: origins(&summary.when_discarded.origins),
            origins_observed: origins(&summary.when_observed.origins),
            opaque_discarded: summary.when_discarded.opaque_parameters.clone(),
            opaque_observed: summary.when_observed.opaque_parameters.clone(),
            opaque_origins_discarded: opaque_origins(&summary.when_discarded.opaque_origins),
            opaque_origins_observed: opaque_origins(&summary.when_observed.opaque_origins),
        })));
        true
    }

    fn opaque_arguments(&self, node: Node<'_>, effects: &mut Vec<Effect>) {
        if !matches!(node.kind(), "call_expression" | "method_invocation" | "new_expression") || self.origin(node).is_some() { return; }
        let Some(arguments) = site::arguments(node) else { return; };
        for argument in site::children(arguments).filter_map(site::argument) {
            let argument = transparent(argument);
            if let Some(place) = self.place(argument, false) {
                effects.push(Effect::OpaqueRead(place));
            } else if self.helper_call(argument).is_some() {
                let temporary = Place::CallResult(argument.start_byte());
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
