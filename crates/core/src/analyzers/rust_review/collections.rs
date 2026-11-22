mod initialization;

use autoreview_schema::Severity;
use tree_sitter::Node;

use super::syntax::{arguments, children, method_call, Source};
use super::{Review, RustReviewRule};

const STRONG_CYCLE: RustReviewRule = RustReviewRule {
    id: "rust-collection-strong-self-cycle",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Collection element holds a strong reference to its own storage",
    message: "The new element stores a clone of the Rc or Arc that owns this collection. The strong cycle prevents automatic destruction while the entry remains. Use a Weak back-reference if the element does not need to own the collection.",
    reference: "https://doc.rust-lang.org/stable/book/ch15-06-reference-cycles.html",
    cwe: None,
};

const FORGOTTEN_DRAIN: RustReviewRule = RustReviewRule {
    id: "rust-forgotten-local-vec-drain",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Forgotten drain can leak vector elements",
    message: "Vec::Drain restores the vector state when it is dropped. Forgetting it can leak elements, including outside the drained range. Drop the drain or use a collection operation that preserves the intended elements.",
    reference: "https://doc.rust-lang.org/std/vec/struct.Vec.html#method.drain",
    cwe: None,
};

pub(super) const UNINITIALIZED_LEN: RustReviewRule = RustReviewRule {
    id: "rust-vec-set-len-before-initialization",
    category: "correctness",
    severity: Severity::High,
    semantic: false,
    title: "Vec length exposes uninitialized elements",
    message: "This vector has capacity but its elements have not been initialized. Initialize each new element before extending its logical length, or use a safe constructor or resize operation.",
    reference: "https://doc.rust-lang.org/std/vec/struct.Vec.html#method.set_len",
    cwe: None,
};

pub(super) const RULES: &[&RustReviewRule] = &[&STRONG_CYCLE, &FORGOTTEN_DRAIN, &UNINITIALIZED_LEN];

pub(super) fn check<'tree>(review: &mut Review<'tree, '_>, node: Node<'tree>) {
    if node.kind() != "call_expression" {
        return;
    }
    if let Some(binding) = strong_cycle(&review.source, node) {
        review.report_related(&STRONG_CYCLE, node, &[binding]);
    }
    if let Some(binding) = forgotten_drain(&review.source, node) {
        review.report_related(&FORGOTTEN_DRAIN, node, &[binding]);
    }
    if let Some(binding) = initialization::uninitialized_length(&review.source, node) {
        review.report_related(&UNINITIALIZED_LEN, node, &[binding]);
    }
}

fn strong_cycle<'tree>(source: &Source<'tree, '_>, call: Node<'tree>) -> Option<Node<'tree>> {
    let borrowed = method_receiver(source, call, "push")?;
    let collection = method_receiver(source, borrowed, "borrow_mut")?;
    let binding = source.local_binding(collection)?;
    if children(binding).any(|node| node.kind() == "mutable_specifier") {
        return None;
    }
    let kind = strong_vec_owner(source, binding.child_by_field_name("value")?)?;
    let element = arguments(call)
        .next()
        .filter(|node| node.kind() == "struct_expression")?;
    let fields = element.child_by_field_name("body")?;
    children(fields)
        .filter_map(|field| field.child_by_field_name("value"))
        .any(|value| strong_clone(source, value, kind) == Some(binding))
        .then_some(binding)
}

fn strong_vec_owner(source: &Source<'_, '_>, constructor: Node<'_>) -> Option<&'static str> {
    if constructor.kind() != "call_expression" {
        return None;
    }
    let kind = match call_path(source, constructor).as_deref()? {
        "std::rc::Rc::new" | "alloc::rc::Rc::new" => "Rc",
        "std::sync::Arc::new" | "alloc::sync::Arc::new" => "Arc",
        _ => return None,
    };
    let cell = arguments(constructor).next()?;
    let cell_path = call_path(source, cell)?;
    if !matches!(
        cell_path.as_str(),
        "std::cell::RefCell::new" | "core::cell::RefCell::new"
    ) {
        return None;
    }
    let initialize_vec = arguments(cell).next()?.child_by_field_name("function")?;
    let owner = initialize_vec.child_by_field_name("path")?;
    let name = initialize_vec.child_by_field_name("name")?;
    (source.text(name) == "new" && standard_vec(source, owner)).then_some(kind)
}

fn strong_clone<'tree>(
    source: &Source<'tree, '_>,
    call: Node<'tree>,
    kind: &str,
) -> Option<Node<'tree>> {
    if let Some(receiver) = method_receiver(source, call, "clone") {
        return source.local_binding(receiver);
    }
    let operation = call_path(source, call)?;
    let matching_clone = match kind {
        "Rc" => matches!(
            operation.as_str(),
            "std::rc::Rc::clone" | "alloc::rc::Rc::clone"
        ),
        _ => matches!(
            operation.as_str(),
            "std::sync::Arc::clone" | "alloc::sync::Arc::clone"
        ),
    };
    let reference = arguments(call).next()?;
    if !matching_clone || reference.kind() != "reference_expression" {
        return None;
    }
    source.local_binding(reference.child_by_field_name("value")?)
}

fn forgotten_drain<'tree>(source: &Source<'tree, '_>, call: Node<'tree>) -> Option<Node<'tree>> {
    let path = call_path(source, call)?;
    if !matches!(path.as_str(), "std::mem::forget" | "core::mem::forget") {
        return None;
    }
    let receiver = method_receiver(source, arguments(call).next()?, "drain")?;
    let declaration = source.declaration(receiver)?;
    let mut ty = declaration.child_by_field_name("type")?;
    while ty.kind() == "reference_type" {
        ty = ty.child_by_field_name("type")?;
    }
    if ty.kind() == "generic_type" {
        ty = ty.child_by_field_name("type")?;
    }
    standard_vec(source, ty).then_some(declaration)
}

fn standard_vec(source: &Source<'_, '_>, node: Node<'_>) -> bool {
    source.unshadowed_prelude(node, "Vec")
        || matches!(
            source.external_path(node).as_deref(),
            Some("std::vec::Vec" | "alloc::vec::Vec")
        )
}

fn call_path(source: &Source<'_, '_>, call: Node<'_>) -> Option<String> {
    source.external_path(call.child_by_field_name("function")?)
}

fn method_receiver<'tree>(
    source: &Source<'tree, '_>,
    call: Node<'tree>,
    name: &str,
) -> Option<Node<'tree>> {
    let (receiver, method) = method_call(call)?;
    (source.text(method) == name).then_some(receiver)
}

#[cfg(test)]
mod tests {
    use super::super::analyze_file;

    #[test]
    fn vec_set_len_checks_fresh_capacity_without_intervening_initialization() {
        let source = r#"
use std::vec::Vec as Buffer;

fn bad() {
    let mut values = Buffer::<u8>::with_capacity(2);
    unsafe { values.set_len(2_usize); }
}

fn initialized() {
    let mut values = Vec::<u8>::with_capacity(1);
    values.spare_capacity_mut()[0].write(42);
    unsafe { values.set_len(1); }
}

fn handed_to_ffi() {
    let mut values = Vec::<u8>::with_capacity(1);
    unsafe { fill(values.as_mut_ptr()); values.set_len(1); }
}

fn empty() {
    let mut values = Vec::<u8>::with_capacity(1);
    unsafe { values.set_len(0); }
}
"#;
        let findings = analyze_file("vec.rs", source);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].source.rule_id.as_deref(), Some(super::UNINITIALIZED_LEN.id));
        assert_eq!(findings[0].meta.as_ref().unwrap()["subcategory"][0], "memory.initialization");
        assert!(findings[0].related_locations.as_ref().unwrap()[0].snippet.contains("with_capacity"));
    }

    #[test]
    fn vec_set_len_requires_a_contiguous_initialized_prefix() {
        let source = r#"
fn partially_initialized() {
    let mut values = Vec::with_capacity(4);
    values.push(1);
    values.spare_capacity_mut()[0].write(2);
    let _len = values.len();
    unsafe { values.set_len(3); }
}
fn initialized_with_gap() {
    let mut values = Vec::with_capacity(4);
    values.resize(1, 0);
    values.spare_capacity_mut()[1].write(1);
    unsafe { values.set_len(3); }
}
fn initialized_contiguously() {
    let mut values = Vec::with_capacity(4);
    values.push(1);
    values.spare_capacity_mut()[1].write(3);
    values.spare_capacity_mut()[0].write(2);
    unsafe { values.set_len(3); }
}
fn captured() {
    let mut values = Vec::with_capacity(4);
    let mut fill = || { values.push(1); };
    fill();
    unsafe { values.set_len(1); }
}
"#;
        let findings = analyze_file("prefix.rs", source);
        assert_eq!(findings.len(), 2, "{findings:#?}");
        assert_eq!(findings.iter().map(|finding| finding.location.range.start_line).collect::<Vec<_>>(), [7, 13]);
    }

    #[test]
    fn detects_self_cycles_through_collection_elements() {
        let code = r#"
use std::{cell::RefCell as Cell, rc::Rc as Ref, rc::Weak};
struct Node { children: Ref<Cell<Vec<Node>>> }
struct WeakNode { children: Weak<Cell<Vec<WeakNode>>> }

fn cycles() {
    let children = Ref::new(Cell::new(Vec::new()));
    children.borrow_mut().push(Node { children: Ref::clone(&children) });
}

fn weak() {
    let children = Ref::new(Cell::new(Vec::new()));
    children.borrow_mut().push(WeakNode { children: Ref::downgrade(&children) });
}

fn independent(other: Ref<Cell<Vec<Node>>>) {
    let children = Ref::new(Cell::new(Vec::new()));
    children.borrow_mut().push(Node { children: other.clone() });
}
"#;
        let found = analyze_file("graph.rs", code);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(
            found[0].source.rule_id.as_deref(),
            Some("rust-collection-strong-self-cycle")
        );
        assert_eq!(
            found[0].related_locations.as_ref().unwrap()[0].snippet,
            "let children = Ref::new(Cell::new(Vec::new()));"
        );
    }

    #[test]
    fn resolves_forgotten_method_drains_to_a_standard_vec() {
        let code = r#"
use std::mem::forget as ignore;
use std::vec::Vec as List;

fn process(values: &mut List<String>, custom: LocalDrain) {
    ignore(values.drain(..2));
    drop(values.drain(..1));
    ignore(custom.drain(..2));
}
"#;
        let found = analyze_file("lists.rs", code);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(
            found[0].source.rule_id.as_deref(),
            Some("rust-forgotten-local-vec-drain")
        );
    }
}
