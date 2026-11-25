use tree_sitter::Node;

use crate::lower::site;

use super::super::references::{reference, string_references};
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
        (arguments.len() == summary.when_discarded.parameters.len()).then_some((arguments, summary))
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
        if let Some((arguments, summary)) = self.helper_call(node) {
            let arguments = arguments
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
                .collect();
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
            effects.push(Effect::ApplyHelper(HelperCall {
                arguments,
                demand,
                when_discarded: summary.when_discarded.parameters.clone(),
                when_observed: summary.when_observed.parameters.clone(),
                origins_discarded: origins(&summary.when_discarded.origins),
                origins_observed: origins(&summary.when_observed.origins),
            }));
            return;
        }
        if let Some(closure) = self.closures.literal(node) {
            let snapshots = closure
                .snapshots
                .iter()
                .map(|&(source, target)| Write { target: Some(target), value: Value::Copy(source) })
                .collect();
            effects.push(Effect::Write(snapshots));
            return;
        }
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
