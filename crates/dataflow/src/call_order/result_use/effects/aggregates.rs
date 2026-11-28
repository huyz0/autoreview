use tree_sitter::Node;

use crate::lower::site;
use crate::lower::rust::aggregate_result_operands;

use super::super::places::operand_slot;
use super::{return_value, transparent, ContractLanguage, Effect, Lower, Value};

impl Lower<'_, '_, '_> {
    pub(super) fn project_match(&self, node: Node<'_>, effects: &mut Vec<Effect>) -> bool {
        if node.kind() != "match_expression" || self.language != ContractLanguage::Rust {
            return false;
        }
        let Some(projections) = crate::lower::rust::literal_projection(node, self.source) else {
            return false;
        };
        let writes = projections.into_iter()
            .filter_map(|(binding, value)| self.write(binding, Some(value), false, effects))
            .collect();
        effects.push(Effect::Write(writes));
        true
    }

    pub(super) fn staged_aggregate(&self, node: Node<'_>) -> bool {
        self.language == ContractLanguage::Rust
            && matches!(node.kind(), "array_expression" | "tuple_expression")
            && (crate::lower::rust::stages_aggregate(node) || self.plan.rust_elements.tracks_aggregate(node.start_byte()))
    }

    pub(super) fn aggregate_value(&self, value: Node<'_>, effects: &mut Vec<Effect>) -> Option<Value> {
        if self.staged_aggregate(value) {
            Some(Value::StagedAggregate(aggregate_result_operands(value, self.source).map(operand_slot).collect()))
        } else if self.language == ContractLanguage::Web && value.kind() == "array" {
            let members = site::children(value).map(|item| {
                let member = self.value(item, effects);
                if let Value::Copy(place) = member {
                    effects.push(Effect::ObservedOnly(vec![Effect::Read(place)]));
                    Value::Copy(place)
                } else {
                    member
                }
            });
            Some(Value::Aggregate(members.collect()))
        } else {
            None
        }
    }

    pub(super) fn read_staged_aggregate(&self, aggregate: Node<'_>, effects: &mut Vec<Effect>) -> bool {
        if !self.staged_aggregate(aggregate) { return false; }
        effects.extend(aggregate_result_operands(aggregate, self.source).map(|item| Effect::OpaqueRead(operand_slot(item))));
        true
    }

    pub(super) fn aggregate_expression(&self, node: Node<'_>, effects: &mut Vec<Effect>) -> bool {
        let value = return_value(node).unwrap_or(node);
        let aggregate = transparent(value);
        if !self.staged_aggregate(aggregate) { return false; }
        if crate::lower::rust::value_is_returned(value) {
            self.observe_elements(aggregate, effects);
        } else if node.parent().is_some_and(|owner| owner.kind() == "let_declaration") {
            self.read_staged_aggregate(aggregate, effects);
        }
        true
    }

    pub(super) fn observe_elements(&self, aggregate: Node<'_>, effects: &mut Vec<Effect>) {
        effects.extend(aggregate_result_operands(aggregate, self.source).map(|item| Effect::Read(operand_slot(item))));
    }
}
