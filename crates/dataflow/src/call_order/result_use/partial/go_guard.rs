use super::{has_conditional, supported_effect, update_aliases, Cfg, Effect, Node, NodeId, Origin, Place, Plan, State, Stmt};
use crate::call_order::ContractLanguage;

pub(super) fn observes(context: (&Cfg<Stmt>, &Plan<'_, '_>, (Node<'_>, &[u8]), NodeId), effects: &[Effect], origin: &Origin, state: &mut State) -> bool {
    let Some(place) = checked_place(context) else { return false; };
    for effect in effects {
        if state.active && !supported_effect(effect) { return false; }
        if matches!(effect, Effect::Read(read) if *read == place && state.active && state.aliases.contains(read)) { return true; }
        if let Effect::Write(writes) = effect { update_aliases(writes, origin, state); }
        if state.active && state.aliases.is_empty() { return false; }
    }
    false
}

fn checked_place(context: (&Cfg<Stmt>, &Plan<'_, '_>, (Node<'_>, &[u8]), NodeId)) -> Option<Place> {
    let (cfg, plan, syntax, node) = context;
    if plan.spec.contract.as_ref()?.language != ContractLanguage::Go { return None; }
    let mut conditional = cfg.nodes[node].evaluations.iter().filter_map(|range| super::super::effects::evaluation(syntax.0, range))
        .filter(|value| has_conditional(*value, syntax.1, false));
    let value = conditional.next()?;
    if conditional.next().is_some() { return None; }
    let identifier = checked_identifier(value)?;
    let name = identifier.utf8_text(syntax.1).ok()?;
    plan.syntax.local_result_binding(name, identifier.start_byte(), false).map(Place::Local)
}

fn checked_identifier(value: Node<'_>) -> Option<Node<'_>> {
    if value.kind() != "binary_expression" || !matches!(value.child_by_field_name("operator")?.kind(), "&&" | "||") { return None; }
    let left = value.child_by_field_name("left").filter(|left| left.kind() == "binary_expression")?;
    if !matches!(left.child_by_field_name("operator")?.kind(), "==" | "!=") { return None; }
    let identifier = left.child_by_field_name("left").filter(|node| node.kind() == "identifier")?;
    (left.child_by_field_name("right")?.kind() == "nil").then_some(identifier)
}
