use std::collections::{BTreeMap, BTreeSet};

use autoreview_schema::Severity;
use tree_sitter::Node;

use super::syntax::{ancestors, children, is_execution_boundary, Source};
use super::{Review, RustReviewRule};

const FEATURE_ENVY: RustReviewRule = RustReviewRule {
    id: "rust-feature-envy",
    category: "design",
    severity: Severity::Low,
    semantic: true,
    title: "Possible Rust feature envy",
    message: "This method accesses several distinct fields of one parameter and uses little of its own state. Check whether the operation belongs with the parameter type.",
    reference: "https://refactoring.com/catalog/moveFunction.html",
    cwe: None,
};

pub(super) const RULES: &[&RustReviewRule] = &[&FEATURE_ENVY];
const MIN_FOREIGN_FIELDS: usize = 3;
const MIN_MARGIN: usize = 2;

pub(super) fn check<'tree>(review: &mut Review<'tree, '_>, function: Node<'tree>) {
    let Some((owner, body)) = method_to_measure(function, &review.source) else {
        return;
    };
    let mut local = BTreeSet::new();
    let mut foreign = BTreeMap::new();
    for field in direct_field_accesses(body) {
        record_access(&review.source, function, field, &mut local, &mut foreign);
    }
    let owner = type_name(&review.source, owner);
    for (_, (parameter, fields)) in foreign {
        if envies_parameter(&review.source, owner, parameter, fields.len(), local.len()) {
            let mut anchors: Vec<_> = fields.into_values().collect();
            anchors.sort_unstable_by_key(Node::start_byte);
            review.report_related(&FEATURE_ENVY, anchors[0], &anchors[1..]);
        }
    }
}

fn method_to_measure<'tree>(
    function: Node<'tree>,
    source: &Source<'tree, '_>,
) -> Option<(Node<'tree>, Node<'tree>)> {
    if function.kind() != "function_item" || in_test(source, function) {
        return None;
    }
    let owner = inherent_owner(function)?;
    let parameters = function.child_by_field_name("parameters")?;
    let has_receiver = children(parameters).any(|param| {
        param.kind() == "self_parameter"
            || param
                .child_by_field_name("pattern")
                .is_some_and(|pattern| pattern.kind() == "self")
    });
    has_receiver.then_some((owner, function.child_by_field_name("body")?))
}

fn in_test(source: &Source<'_, '_>, function: Node<'_>) -> bool {
    std::iter::once(function)
        .chain(ancestors(function))
        .filter(|node| matches!(node.kind(), "function_item" | "impl_item" | "mod_item"))
        .any(|item| source.is_test_item(item))
}

fn inherent_owner(function: Node<'_>) -> Option<Node<'_>> {
    let implementation = function.parent()?.parent()?;
    (implementation.kind() == "impl_item" && implementation.child_by_field_name("trait").is_none())
        .then(|| implementation.child_by_field_name("type"))
        .flatten()
}

fn envies_parameter(
    source: &Source<'_, '_>,
    owner: Option<&str>,
    parameter: Node<'_>,
    foreign: usize,
    local: usize,
) -> bool {
    let target = parameter
        .child_by_field_name("type")
        .and_then(|ty| type_name(source, ty));
    let distinct = owner
        .zip(target)
        .is_some_and(|(owner, target)| target != owner && target != "Self");
    distinct && foreign >= MIN_FOREIGN_FIELDS && foreign >= local + MIN_MARGIN
}

fn type_name<'t>(source: &Source<'_, 't>, ty: Node<'_>) -> Option<&'t str> {
    match ty.kind() {
        "reference_type" | "generic_type" => type_name(source, ty.child_by_field_name("type")?),
        "scoped_type_identifier" => Some(source.text(ty.child_by_field_name("name")?)),
        "type_identifier" => Some(source.text(ty)),
        _ => None,
    }
}

fn direct_field_accesses(body: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut pending = vec![body];
    std::iter::from_fn(move || {
        while let Some(node) = pending.pop() {
            if is_execution_boundary(node) {
                continue;
            }
            let start = pending.len();
            pending.extend(children(node));
            pending[start..].reverse();
            if node.kind() == "field_expression" {
                return Some(node);
            }
        }
        None
    })
}

type ForeignFields<'tree, 'text> =
    BTreeMap<usize, (Node<'tree>, BTreeMap<&'text str, Node<'tree>>)>;

fn record_access<'tree, 'text>(
    source: &Source<'tree, 'text>,
    function: Node<'tree>,
    field: Node<'tree>,
    local: &mut BTreeSet<&'text str>,
    foreign: &mut ForeignFields<'tree, 'text>,
) {
    let Some((receiver, member)) = field
        .child_by_field_name("value")
        .zip(field.child_by_field_name("field"))
    else {
        return;
    };
    if receiver.kind() == "self" {
        local.insert(source.text(member));
        return;
    }
    if is_method_callee(field) {
        return;
    }
    let Some(parameter) = source.declaration(receiver).filter(|parameter| {
        parameter.kind() == "parameter"
            && parameter.parent().and_then(|list| list.parent()) == Some(function)
    }) else {
        return;
    };
    foreign
        .entry(parameter.start_byte())
        .or_insert_with(|| (parameter, BTreeMap::new()))
        .1
        .entry(source.text(member))
        .or_insert(field);
}

fn is_method_callee(field: Node<'_>) -> bool {
    let Some(parent) = field.parent() else {
        return false;
    };
    matches!(parent.kind(), "call_expression" | "generic_function")
        && parent.child_by_field_name("function") == Some(field)
}

#[cfg(test)]
mod tests {
    use super::super::analyze_file;

    #[test]
    fn flags_distinct_fields_on_one_typed_parameter_with_evidence() {
        let findings = analyze_file("src/lib.rs", "impl Checkout { fn total(&self, cart: &shop::Cart) -> i32 { cart.base + cart.tax + cart.fee + self.rounding } }");
        let envy = findings
            .iter()
            .find(|finding| finding.source.rule_id.as_deref() == Some("rust-feature-envy"))
            .unwrap();
        assert_eq!(envy.related_locations.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn ignores_adapters_repeats_shadowing_and_other_execution_contexts() {
        let source = r#"
impl Checkout {
    fn same_type(&self, other: &Self) { let _ = other.a + other.b + other.c; }
    fn own_state(&self, cart: &Cart) { let _ = cart.a + cart.b + cart.c + self.a + self.b; }
    fn delegates(&self, cart: &Cart) { cart.a(); cart.b(); cart.c(); }
    fn repeated(&self, cart: &Cart) { let _ = cart.a + cart.a + cart.a; }
    fn separate(&self, one: &Cart, two: &Cart) { let _ = one.a + two.b + two.c; }
    fn shadow(&self, cart: &Cart) { let cart = other(); let _ = cart.a + cart.b + cart.c; }
    fn closure(&self, cart: &Cart) { let _ = || cart.a + cart.b + cart.c; }
    fn associated(cart: &Cart) { let _ = cart.a + cart.b + cart.c; }
}
impl Adapter for Checkout { fn convert(&self, cart: &Cart) { let _ = cart.a + cart.b + cart.c; } }
#[cfg(test)]
mod tests { impl Checkout { fn helper(&self, cart: &Cart) { let _ = cart.a + cart.b + cart.c; } } }
"#;
        let findings = analyze_file("src/lib.rs", source);
        assert!(
            findings
                .iter()
                .all(|finding| finding.source.rule_id.as_deref() != Some("rust-feature-envy")),
            "{findings:?}"
        );
    }
}
