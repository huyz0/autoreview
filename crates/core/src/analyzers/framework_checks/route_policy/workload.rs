use autoreview_schema::{artifact_config::RouteWorkloadLimit, AgentFinding};

use super::super::web::node_calls::string_literal;
use super::super::web::string_flow::meaningful_children;
use super::super::{text, Node};
use super::{guards::argument, Review, Rule, AUTHORIZATION};

pub(in crate::analyzers) const LIMIT: Rule = Rule {
    id: "express-route-workload-limit-required",
    subcategory: "resources.workload-limit",
    title: "Route operation has no acceptable explicit workload limit",
    reference:
        "https://owasp.org/API-Security/editions/2023/en/0xa4-unrestricted-resource-consumption/",
    ..AUTHORIZATION
};

pub(super) fn findings(
    review: &Review<'_, '_, '_>,
    policy: &RouteWorkloadLimit,
) -> Vec<AgentFinding> {
    review
        .direct_calls(&policy.operation)
        .into_iter()
        .filter_map(|call| {
            let options =
                argument(call, policy.options_argument).filter(|value| value.kind() == "object")?;
            let limit = limit_value(options, review.source, &policy.limit_field)?;
            let description = match limit {
                None => format!("omits {}", policy.limit_field),
                Some(limit) if limit < 1 || limit > i128::from(policy.maximum) => {
                    format!("sets {} to {limit}", policy.limit_field)
                }
                _ => return None,
            };
            Some(review.finding(
                &LIMIT,
                call,
                format!(
                    "This call {description}. Supply a limit from 1 to {} for this route.",
                    policy.maximum
                ),
            ))
        })
        .collect()
}

fn limit_value(options: Node<'_>, source: &str, field: &str) -> Option<Option<i128>> {
    let mut limit = None;
    for property in meaningful_children(options) {
        let key = property.child_by_field_name("key")?;
        let name = match key.kind() {
            "property_identifier" | "number" => text(key, source),
            "string" => string_literal(key, source)?,
            _ => return None,
        };
        if name == field {
            limit = Some(property.child_by_field_name("value")?);
        }
    }
    match limit {
        None => Some(None),
        Some(node) => decimal_integer(node, source).map(Some),
    }
}

fn decimal_integer(node: Node<'_>, source: &str) -> Option<i128> {
    let (number, sign) = match node.kind() {
        "number" => (node, 1),
        "unary_expression" if text(node.child_by_field_name("operator")?, source) == "-" => {
            (node.child_by_field_name("argument")?, -1)
        }
        _ => return None,
    };
    if number.kind() != "number" {
        return None;
    }
    text(number, source)
        .replace('_', "")
        .parse::<i128>()
        .ok()
        .and_then(|number| number.checked_mul(sign))
}
