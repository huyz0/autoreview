use std::collections::BTreeSet;

use tree_sitter::Node;

use super::super::syntax::{
    ancestors, arguments, attributes, children, descendants, method_call, Source,
};
use super::{method_receiver, standard_vec};

pub(super) fn uninitialized_length<'t>(
    source: &Source<'t, '_>,
    call: Node<'t>,
) -> Option<Node<'t>> {
    let receiver = method_receiver(source, call, "set_len")?;
    let target_len = arguments(call)
        .next()
        .and_then(|length| size(source, length))?;
    if target_len == 0 {
        return None;
    }
    let binding = source
        .local_binding(receiver)
        .filter(|node| fresh_capacity(source, *node))?;
    let scope = binding.parent().filter(|parent| parent.kind() == "block")?;
    if ancestors(call)
        .take_while(|node| *node != scope)
        .any(repeats_execution)
    {
        return None;
    }
    let state = initialized_before(source, scope, binding, receiver)?;
    (!state.has_prefix(target_len)).then_some(binding)
}

fn fresh_capacity(source: &Source<'_, '_>, binding: Node<'_>) -> bool {
    let constructor = (|| {
        let initializer = binding
            .child_by_field_name("value")
            .filter(|node| node.kind() == "call_expression")?;
        let constructor = initializer.child_by_field_name("function")?;
        let owner = constructor.child_by_field_name("path")?;
        let owner = if owner.kind() == "generic_type" {
            owner.child_by_field_name("type")?
        } else {
            owner
        };
        Some((owner, constructor.child_by_field_name("name")?))
    })();
    constructor.is_some_and(|(owner, name)| {
        source.text(name) == "with_capacity" && standard_vec(source, owner)
    })
}

fn initialized_before<'t>(
    source: &Source<'t, '_>,
    scope: Node<'t>,
    binding: Node<'t>,
    receiver: Node<'t>,
) -> Option<Initialized> {
    let references = descendants(scope).filter(|node| {
        node.start_byte() >= binding.end_byte()
            && node.end_byte() <= receiver.start_byte()
            && node.kind() == "identifier"
            && source.text(*node) == source.text(receiver)
            && source
                .local_binding(*node)
                .is_none_or(|resolved| resolved == binding)
    });
    let mut state = Initialized::default();
    for reference in references {
        let effect = vector_effect(source, reference)?;
        if !matches!(effect, Effect::Observe) && !straight_line(reference, scope) {
            // A loop or branch can change how many writes reach this call.
            return None;
        }
        state.apply(effect)?;
    }
    Some(state)
}

#[derive(Default)]
struct Initialized {
    len: usize,
    spare: BTreeSet<usize>,
}

impl Initialized {
    fn apply(&mut self, effect: Effect) -> Option<()> {
        match effect {
            Effect::Observe => {}
            Effect::Push => self.len = self.len.checked_add(1)?,
            Effect::Resize(new_len) => {
                self.spare
                    .retain(|slot| !(new_len..self.len).contains(slot));
                self.len = new_len;
            }
            Effect::Spare(offset) => {
                self.spare.insert(self.len.checked_add(offset)?);
            }
        }
        Some(())
    }

    fn has_prefix(&self, target: usize) -> bool {
        if target <= self.len {
            return true;
        }
        let mut next = self.len;
        for &slot in self.spare.range(self.len..target) {
            if slot != next {
                return false;
            }
            next += 1;
        }
        next == target
    }
}

enum Effect {
    Observe,
    Push,
    Resize(usize),
    Spare(usize),
}

fn vector_effect(source: &Source<'_, '_>, reference: Node<'_>) -> Option<Effect> {
    let call = receiver_call(reference)?;
    let (_, method) = method_call(call)?;
    let method_name = source.text(method);
    match method_name {
        "len" | "capacity" | "is_empty" => Some(Effect::Observe),
        "push" => Some(Effect::Push),
        "resize" | "resize_with" => Some(Effect::Resize(size(source, arguments(call).next()?)?)),
        "spare_capacity_mut" => spare_write(source, call).map(Effect::Spare),
        _ => None,
    }
}

fn receiver_call(node: Node<'_>) -> Option<Node<'_>> {
    let field = node
        .parent()
        .filter(|parent| parent.kind() == "field_expression")?;
    let call = field.parent()?;
    let (receiver, _) = method_call(call)?;
    (receiver == node).then_some(call)
}

fn spare_write(source: &Source<'_, '_>, capacity: Node<'_>) -> Option<usize> {
    let index = capacity
        .parent()
        .filter(|node| node.kind() == "index_expression")?;
    let mut operands = children(index);
    if operands.next()? != capacity {
        return None;
    }
    let slot = size(source, operands.next()?)?;
    let call = receiver_call(index)?;
    let (_, method) = method_call(call)?;
    (source.text(method) == "write").then_some(slot)
}

fn size(source: &Source<'_, '_>, node: Node<'_>) -> Option<usize> {
    if node.kind() != "integer_literal" {
        return None;
    }
    let literal = source.text(node);
    literal
        .strip_suffix("usize")
        .unwrap_or(literal)
        .replace('_', "")
        .parse()
        .ok()
}

fn straight_line(reference: Node<'_>, scope: Node<'_>) -> bool {
    ancestors(reference)
        .take_while(|node| *node != scope)
        .all(|node| {
            !repeats_execution(node)
                && !matches!(
                    node.kind(),
                    "if_expression" | "match_expression" | "binary_expression"
                )
                && attributes(node).next().is_none()
        })
}

fn repeats_execution(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "closure_expression"
            | "async_block"
            | "loop_expression"
            | "while_expression"
            | "for_expression"
    )
}
