use autoreview_schema::Severity;
use tree_sitter::Node;

use super::syntax::{
    arguments, attributes, children, descendants, is_comment, method_call, Source,
};
use super::{Review, RustReviewRule};

const SHADOWED_EXPECTATION: RustReviewRule = RustReviewRule {
    id: "rust-mockall-fallback-shadows-expectation",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Mockall fallback shadows a specific expectation",
    message: "Mockall checks expectations in setup order. This earlier fallback can match every call and has no limit. Put the specific expectation before the fallback.",
    reference: "https://docs.rs/mockall/latest/mockall/#matching-multiple-calls",
    cwe: None,
};

const TEMPORARY_CONTEXT: RustReviewRule = RustReviewRule {
    id: "rust-mockall-temporary-static-context",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Temporary Mockall context drops before the static call",
    message: "This context drops at the end of the setup statement, which clears its static expectations. Bind the context to a local that stays alive through the call.",
    reference: "https://docs.rs/mockall/latest/mockall/#context-checkpoints",
    cwe: None,
};

pub(super) const RULES: &[&RustReviewRule] = &[&SHADOWED_EXPECTATION, &TEMPORARY_CONTEXT];

pub(super) fn check<'tree>(review: &mut Review<'tree, '_>, node: Node<'tree>) {
    if node.kind() != "expression_statement" {
        return;
    }
    let Some(setup) = expectation(&review.source, node) else {
        return;
    };
    let Some(body) = node.parent().filter(|node| node.kind() == "block") else {
        return;
    };
    let Some(owner) = body.parent().filter(|node| node.kind() == "function_item") else {
        return;
    };
    let Some(module) = owner.parent().filter(|node| {
        node.kind() == "source_file"
            || node.kind() == "declaration_list"
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "mod_item")
    }) else {
        return;
    };
    if owner.child_by_field_name("type_parameters").is_some()
        || attributes(owner).any(|attr| {
            descendants(attr).any(|node| {
                node.kind() == "identifier" && review.source.text(node) == "should_panic"
            })
        })
    {
        return;
    }
    if let Some(method) = setup.method {
        if let Some((fallback, binding)) =
            shadowing_fallback(&review.source, node, setup, method, body, module)
        {
            review.report_related(&SHADOWED_EXPECTATION, node, &[fallback, binding]);
        }
    } else if let Some(call) =
        call_after_temporary_context(&review.source, node, setup.receiver, body, module)
    {
        review.report_related(&TEMPORARY_CONTEXT, node, &[call]);
    }
}

struct Expectation<'tree, 'text> {
    receiver: Node<'tree>,
    method: Option<&'text str>,
    has_matcher: bool,
    has_call_control: bool,
}

fn expectation<'tree, 'text>(
    source: &Source<'tree, 'text>,
    statement: Node<'tree>,
) -> Option<Expectation<'tree, 'text>> {
    let (mut setup, terminal) = method_call(statement_value(statement)?)?;
    if !matches!(
        source.text(terminal),
        "returning" | "returning_st" | "return_const" | "return_const_st"
    ) {
        return None;
    }
    let mut has_matcher = false;
    let mut has_call_control = false;
    while let Some((receiver, name)) = method_call(setup) {
        let name = source.text(name);
        match name {
            "with" | "withf" | "withf_st" => has_matcher = true,
            "times" | "once" | "never" | "in_sequence" => has_call_control = true,
            _ => {
                let method = if name == "expect" {
                    None
                } else {
                    Some(name.strip_prefix("expect_")?)
                };
                return arguments(setup).next().is_none().then_some(Expectation {
                    receiver,
                    method,
                    has_matcher,
                    has_call_control,
                });
            }
        }
        setup = receiver;
    }
    None
}

fn shadowing_fallback<'tree>(
    source: &Source<'tree, '_>,
    statement: Node<'tree>,
    specific: Expectation<'tree, '_>,
    method: &str,
    body: Node<'tree>,
    module: Node<'tree>,
) -> Option<(Node<'tree>, Node<'tree>)> {
    if !specific.has_matcher {
        return None;
    }
    let fallback_statement = previous_statement(statement)?;
    if fallback_statement.kind() != "expression_statement" {
        return None;
    }
    let fallback = expectation(source, fallback_statement)?;
    if fallback.has_call_control || fallback.has_matcher || fallback.method != Some(method) {
        return None;
    }
    let binding = previous_statement(fallback_statement)?;
    if Some(binding) != source.local_binding(specific.receiver)
        || Some(binding) != source.local_binding(fallback.receiver)
    {
        return None;
    }
    let constructor = binding.child_by_field_name("value")?;
    let (mock_type, factory) = associated_call(source, constructor)?;
    (factory == "new"
        && arguments(constructor).next().is_none()
        && generated_mock_method(source, mock_type, method, false, body, module))
    .then_some((fallback_statement, binding))
}

fn call_after_temporary_context<'tree>(
    source: &Source<'tree, '_>,
    statement: Node<'tree>,
    context_call: Node<'tree>,
    body: Node<'tree>,
    module: Node<'tree>,
) -> Option<Node<'tree>> {
    if arguments(context_call).next().is_some() {
        return None;
    }
    let (mock_type, factory) = associated_call(source, context_call)?;
    let method = factory.strip_suffix("_context")?;
    let next = std::iter::successors(statement.next_named_sibling(), |node| {
        node.next_named_sibling()
    })
    .find(|node| !is_comment(*node))?;
    let (called_type, called_method) = associated_call(source, statement_value(next)?)?;
    (called_type == mock_type
        && called_method == method
        && generated_mock_method(source, mock_type, method, true, body, module))
    .then_some(next)
}

fn previous_statement(node: Node<'_>) -> Option<Node<'_>> {
    std::iter::successors(node.prev_named_sibling(), |node| node.prev_named_sibling())
        .find(|node| !is_comment(*node))
}

fn statement_value(statement: Node<'_>) -> Option<Node<'_>> {
    match statement.kind() {
        "expression_statement" => children(statement).find(|node| !is_comment(*node)),
        "let_declaration" => statement.child_by_field_name("value"),
        _ => None,
    }
}

fn associated_call<'tree, 'text>(
    source: &Source<'tree, 'text>,
    call: Node<'tree>,
) -> Option<(&'text str, &'text str)> {
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call
        .child_by_field_name("function")
        .filter(|node| node.kind() == "scoped_identifier")?;
    let qualifier = function
        .child_by_field_name("path")
        .filter(|node| node.kind() == "identifier")?;
    let method = function.child_by_field_name("name")?;
    Some((source.text(qualifier), source.text(method)))
}

fn generated_mock_method<'tree>(
    source: &Source<'tree, '_>,
    mock_type: &str,
    method: &str,
    is_static: bool,
    body: Node<'tree>,
    module: Node<'tree>,
) -> bool {
    let Some(trait_name) = mock_type.strip_prefix("Mock") else {
        return false;
    };
    if [body, module].into_iter().any(|scope| {
        children(scope).any(|item| match item.kind() {
            "use_declaration" => descendants(item).any(|part| {
                part.kind() == "identifier" && source.text(part) == mock_type
                    || scope == body && part.kind() == "use_wildcard"
            }),
            "struct_item"
            | "enum_item"
            | "union_item"
            | "type_item"
            | "trait_item"
            | "mod_item"
            | "extern_crate_declaration" => item
                .child_by_field_name("alias")
                .or_else(|| item.child_by_field_name("name"))
                .is_some_and(|name| source.text(name) == mock_type),
            _ => false,
        })
    }) {
        return false;
    }
    children(module).any(|item| {
        item.kind() == "trait_item"
            && item
                .child_by_field_name("name")
                .is_some_and(|name| source.text(name) == trait_name)
            && item.child_by_field_name("type_parameters").is_none()
            && !is_conditional(source, item)
            && attributes(item).any(|attr| generates_named_mock(source, attr))
            && item.child_by_field_name("body").is_some_and(|body| {
                // Mockall omits its constructor when the mocked trait defines new.
                if !is_static
                    && children(body).any(|item| {
                        matches!(item.kind(), "function_signature_item" | "function_item")
                            && item
                                .child_by_field_name("name")
                                .is_some_and(|name| matches!(source.text(name), "new" | "r#new"))
                    })
                {
                    return false;
                }
                children(body).any(|declaration| {
                    matches!(
                        declaration.kind(),
                        "function_signature_item" | "function_item"
                    ) && declaration
                        .child_by_field_name("name")
                        .is_some_and(|name| source.text(name) == method)
                        && declaration.child_by_field_name("type_parameters").is_none()
                        && !is_conditional(source, declaration)
                        && declaration
                            .child_by_field_name("parameters")
                            .is_some_and(|params| {
                                is_static
                                    != children(params).any(|param| {
                                        param.kind() == "self_parameter"
                                            || param
                                                .child_by_field_name("pattern")
                                                .is_some_and(|pattern| pattern.kind() == "self")
                                    })
                            })
                })
            })
    })
}

fn generates_named_mock<'tree>(source: &Source<'tree, '_>, attribute: Node<'tree>) -> bool {
    let mut parts = children(attribute);
    let Some(name) = parts.next() else {
        return false;
    };
    if source.external_path(name).as_deref() != Some("mockall::automock") {
        return false;
    }
    !parts
        .filter(|part| part.kind() == "token_tree")
        .any(|options| {
            children(options).any(|option| {
                option.kind() == "identifier"
                    && matches!(source.text(option), "target" | "r#target")
                    && std::iter::successors(option.next_sibling(), |token| token.next_sibling())
                        .find(|token| !is_comment(*token))
                        .is_some_and(|token| token.kind() == "=")
            })
        })
}

fn is_conditional(source: &Source<'_, '_>, item: Node<'_>) -> bool {
    attributes(item).any(|attr| {
        children(attr)
            .next()
            .is_some_and(|name| matches!(source.text(name), "cfg" | "cfg_attr"))
    })
}

#[cfg(test)]
mod tests {
    use super::super::analyze_file;

    #[test]
    fn skips_a_mocked_static_new_that_returns_a_different_type() {
        let findings = analyze_file(
            "lib.rs",
            r#"
#[mockall::automock]
trait Store {
    fn new() -> OtherDouble;
    fn load(&self, key: u64) -> u64;
}

struct OtherDouble;
impl OtherDouble {
    fn expect_load(&mut self) -> &mut Self { self }
    fn withf(&mut self, _: impl Fn(&u64) -> bool) -> &mut Self { self }
    fn return_const(&mut self, _: u64) {}
}

#[test]
fn calls_mocked_new() {
    let context = MockStore::new_context();
    context.expect().returning(|| OtherDouble);
    let mut mock = MockStore::new();
    mock.expect_load().return_const(0);
    mock.expect_load().withf(|key| *key == 1).return_const(2);
}
"#,
        );
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[test]
    fn skips_automock_with_an_alternate_target() {
        let findings = analyze_file(
            "lib.rs",
            r#"
#[mockall::automock(target /* a generated trait */ = AlternateStore)]
#[trait_variant::make(AlternateStore: Send)]
trait Store {
    fn load(&self, key: u64) -> u64;
    fn now() -> u64;
}

macro_rules! manual_double {
    () => {
        struct MockStore;
        impl MockStore {
            fn new() -> Self { Self }
            fn expect_load(&mut self) -> &mut Self { self }
            fn withf(&mut self, _: impl Fn(&u64) -> bool) -> &mut Self { self }
            fn return_const(&mut self, _: u64) {}
            fn now_context() -> Self { Self }
            fn expect(&mut self) -> &mut Self { self }
            fn now() -> u64 { 1 }
        }
    };
}
manual_double!();

#[test]
fn uses_the_manual_double() {
    let mut mock = MockStore::new();
    mock.expect_load().return_const(0);
    mock.expect_load().withf(|key| *key == 1).return_const(2);
    MockStore::now_context().expect().return_const(1);
    MockStore::now();
}
"#,
        );
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[test]
    fn reports_only_the_unbounded_fallback_on_the_same_generated_mock() {
        let findings = analyze_file(
            "lib.rs",
            r#"
use mockall::automock as make_mock;
#[make_mock]
trait Store { fn load(&self, key: u64) -> u64; }

#[test]
fn unbounded() {
    let mut mock = MockStore::new();
    mock.expect_load().return_const(0_u64);
    mock.expect_load().withf(|key| *key == 1).once().return_const(2_u64);
}
#[test]
fn bounded() {
    let mut mock = MockStore::new();
    mock.expect_load().once().return_const(0_u64);
    mock.expect_load().withf(|key| *key == 1).return_const(2_u64);
}
#[test]
fn reversed() {
    let mut mock = MockStore::new();
    mock.expect_load().withf(|key| *key == 1).return_const(2_u64);
    mock.expect_load().return_const(0_u64);
}
#[test]
fn shadowed() {
    let mut mock = MockStore::new();
    mock.expect_load().return_const(0_u64);
    let mut mock = MockStore::new();
    mock.expect_load().withf(|key| *key == 1).return_const(2_u64);
}
#[test]
fn checkpoint() {
    let mut mock = MockStore::new();
    mock.expect_load().return_const(0_u64);
    mock.checkpoint();
    mock.expect_load().withf(|key| *key == 1).return_const(2_u64);
}
#[test]
fn different_receiver() {
    let mut first = MockStore::new();
    let mut second = MockStore::new();
    first.expect_load().return_const(0_u64);
    second.expect_load().withf(|key| *key == 1).return_const(2_u64);
}
"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].source.rule_id.as_deref(),
            Some(super::SHADOWED_EXPECTATION.id)
        );
        assert_eq!(
            findings[0].related_locations.as_ref().unwrap()[0]
                .range
                .start_line,
            9
        );
    }

    #[test]
    fn reports_only_a_temporary_context_followed_by_its_mocked_static_call() {
        let findings = analyze_file(
            "lib.rs",
            r#"
#[mockall::automock]
trait Clock { fn now() -> u64; fn tick() -> u64; }

#[test]
fn lost() {
    MockClock::now_context().expect().return_const(1_u64);
    let now = MockClock::now();
    assert_eq!(now, 1);
}
#[test]
fn kept() {
    let context = MockClock::now_context();
    context.expect().return_const(1_u64);
    let now = MockClock::now();
    assert_eq!(now, 1);
}
#[test]
#[should_panic(expected = "No matching expectation")]
fn intentional() {
    MockClock::now_context().expect().return_const(1_u64);
    MockClock::now();
}
#[test]
fn other_method() {
    let context = MockClock::tick_context();
    context.expect().return_const(2_u64);
    MockClock::now_context().expect().return_const(1_u64);
    let tick = MockClock::tick();
    assert_eq!(tick, 2);
}
#[test]
fn local_type_shadows_generated_mock() {
    use manual::MockClock;
    MockClock::now_context().expect().return_const(1_u64);
    MockClock::now();
}
mod manual {
    pub struct MockClock;
    impl MockClock {
        pub fn now_context() -> Self { Self }
        pub fn expect(self) -> Self { self }
        pub fn return_const(self, _: u64) {}
        pub fn now() -> u64 { 1 }
    }
}
"#,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].source.rule_id.as_deref(),
            Some(super::TEMPORARY_CONTEXT.id)
        );
        assert_eq!(
            findings[0].related_locations.as_ref().unwrap()[0]
                .range
                .start_line,
            8
        );
    }
}
