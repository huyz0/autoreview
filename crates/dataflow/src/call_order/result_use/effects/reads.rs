use tree_sitter::Node;

use crate::lower::site;

use super::super::references::{reference, string_references};
use super::{Demand, Effect, Lower, Place, Value, Write};

enum Visit<'tree> {
    Syntax(Node<'tree>, Demand),
    Invoke(usize, Demand),
}

impl Lower<'_, '_, '_> {
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
