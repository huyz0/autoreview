use autoreview_schema::Severity;
use tree_sitter::Node;

use super::syntax::{
    ancestors, arguments, attributes, children, descendants, is_comment, is_execution_boundary,
    method_call, Source,
};
use super::{Review, RustReviewRule};

const WALL_CLOCK: RustReviewRule = RustReviewRule {
    id: "rust-wall-clock-elapsed-unwrap",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Unwrapped wall-clock elapsed time",
    message: "SystemTime can move backwards. Unwrapping elapsed time can panic after a clock adjustment. Use Instant when measuring an interval.",
    reference: "https://doc.rust-lang.org/std/time/struct.SystemTime.html#method.elapsed",
    cwe: None,
};
const DISCARDED_MATCHES: RustReviewRule = RustReviewRule {
    id: "rust-discarded-test-matches",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Discarded test predicate",
    message: "matches! returns a boolean. This test discards it, so a mismatch cannot fail the test. Assert the predicate or inspect its value in an explicit check.",
    reference: "https://doc.rust-lang.org/std/macro.matches.html",
    cwe: None,
};
const DETACHED_ASSERTION: RustReviewRule = RustReviewRule {
    id: "rust-detached-test-assertion",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Detached background assertion",
    message: "This test discards the join handle for a task containing an assertion. A task panic can go unnoticed. Await the handle and check its result.",
    reference: "https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html",
    cwe: None,
};
const DISCARDED_JOIN: RustReviewRule = RustReviewRule {
    id: "rust-discarded-test-join-error",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Discarded test task failure",
    message: "This test awaits an assertion task but discards its JoinError. Propagate the error or unwrap the join result.",
    reference: "https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html",
    cwe: None,
};
const BLOCK_IN_PLACE: RustReviewRule = RustReviewRule {
    id: "rust-block-in-place-current-thread",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Blocking section on Tokio's current-thread runtime",
    message: "block_in_place panics on the current-thread runtime. Use an async operation or move synchronous work to spawn_blocking.",
    reference: "https://docs.rs/tokio/latest/tokio/task/fn.block_in_place.html#panics",
    cwe: None,
};
const TRACE_GUARD: RustReviewRule = RustReviewRule {
    id: "rust-tracing-guard-across-await",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Entered tracing span held across await",
    message: "The guard keeps this span entered while the task yields. Use Instrument on the future or scope the guard to synchronous work.",
    reference: "https://docs.rs/tracing/latest/tracing/struct.Span.html#method.enter",
    cwe: None,
};
const OWNED_TASK_TIMEOUT: RustReviewRule = RustReviewRule {
    id: "rust-timeout-consumes-join-handle",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Timeout consumes a spawned task's join handle",
    message: "The timeout can drop this handle while the task keeps running. Pass a mutable borrow of the handle if you need to abort or join the task on expiry.",
    reference: "https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html",
    cwe: None,
};

pub(super) const RULES: &[&RustReviewRule] = &[
    &WALL_CLOCK,
    &DISCARDED_MATCHES,
    &DETACHED_ASSERTION,
    &DISCARDED_JOIN,
    &BLOCK_IN_PLACE,
    &TRACE_GUARD,
    &OWNED_TASK_TIMEOUT,
];

pub(super) fn check<'tree>(review: &mut Review<'tree, '_>, node: Node<'tree>) {
    match node.kind() {
        "await_expression" => {
            if let Some(binding) = owned_task_timeout(&review.source, node) {
                review.report_related(&OWNED_TASK_TIMEOUT, node, &[binding]);
            }
        }
        "let_declaration" => check_binding(review, node),
        _ => {
            if let Some(rule) = local_expression_rule(&review.source, node) {
                review.report(rule, node);
            }
        }
    }
}

fn check_binding<'tree>(review: &mut Review<'tree, '_>, binding: Node<'tree>) {
    if let Some(awaited) = await_with_live_trace_guard(&review.source, binding) {
        review.report_related(&TRACE_GUARD, binding, &[awaited]);
    }
    if discards_assertion_task(&review.source, binding, true) {
        review.report(&DISCARDED_JOIN, binding);
    }
}

fn local_expression_rule<'tree>(
    source: &Source<'tree, '_>,
    node: Node<'tree>,
) -> Option<&'static RustReviewRule> {
    match node.kind() {
        "call_expression" if wall_clock_unwrapped(source, node) => Some(&WALL_CLOCK),
        "call_expression" if blocks_current_thread(source, node) => Some(&BLOCK_IN_PLACE),
        "expression_statement" if discards_test_matches(source, node) => Some(&DISCARDED_MATCHES),
        "expression_statement" if discards_assertion_task(source, node, false) => Some(&DETACHED_ASSERTION),
        _ => None,
    }
}

fn owned_task_timeout<'tree>(
    source: &Source<'tree, '_>,
    await_expr: Node<'tree>,
) -> Option<Node<'tree>> {
    let call = children(await_expr).find(|node| !is_comment(*node))?;
    let timeout = source.external_path(call.child_by_field_name("function")?)?;
    if !matches!(timeout.as_str(), "tokio::time::timeout" | "tokio::time::timeout_at") {
        return None;
    }
    let binding = source.local_binding(arguments(call).nth(1)?)?;
    if children(binding).any(|node| node.kind() == "mutable_specifier") {
        return None;
    }
    let initializer = binding.child_by_field_name("value")?;
    let spawn = source.external_path(initializer.child_by_field_name("function")?)?;
    matches!(spawn.as_str(), "tokio::spawn" | "tokio::task::spawn" | "tokio::task::spawn_blocking")
        .then_some(binding)
}

fn discards_test_matches<'tree>(source: &Source<'tree, '_>, node: Node<'tree>) -> bool {
    children(node)
        .find(|part| !is_comment(*part))
        .filter(|part| part.kind() == "macro_invocation")
        .and_then(|part| part.child_by_field_name("macro"))
        .is_some_and(|name| source.standard_macro(name, &["matches"]))
        && test_body(source, node)
}

pub(super) fn test_body<'tree>(source: &Source<'tree, '_>, node: Node<'tree>) -> bool {
    let Some(function) = function_owner(node) else {
        return false;
    };
    attributes(function).any(|attr| {
        matches!(
            attr_name(source, attr).as_deref(),
            Some("test" | "tokio::test")
        )
    }) && !attributes(function)
        .any(|attr| attr_name(source, attr).as_deref() == Some("should_panic"))
}

fn await_with_live_trace_guard<'tree>(
    source: &Source<'tree, '_>,
    binding: Node<'tree>,
) -> Option<Node<'tree>> {
    let pattern = binding.child_by_field_name("pattern")?;
    if pattern.kind() != "identifier" {
        return None;
    }
    let (span, method) = method_call(binding.child_by_field_name("value")?)?;
    let method = source.text(method);
    let macro_call = match (method, span.kind()) {
        ("entered", "macro_invocation") => span,
        ("enter" | "entered", "identifier") => {
            let span_binding = source.local_binding(span)?;
            if children(span_binding).any(|part| part.kind() == "mutable_specifier") {
                return None;
            }
            span_binding.child_by_field_name("value")?
        }
        _ => return None,
    };
    let name = source.external_path(macro_call.child_by_field_name("macro")?)?;
    if !matches!(
        name.as_str(),
        "tracing::span"
            | "tracing::trace_span"
            | "tracing::debug_span"
            | "tracing::info_span"
            | "tracing::warn_span"
            | "tracing::error_span"
    ) {
        return None;
    }
    let next = std::iter::successors(binding.next_named_sibling(), |sibling| {
        sibling.next_named_sibling()
    })
    .find(|part| !is_comment(*part))?;
    let mut awaited = match next.kind() {
        "expression_statement" => children(next).find(|part| !is_comment(*part))?,
        "let_declaration" => next.child_by_field_name("value")?,
        "await_expression" | "try_expression" => next,
        _ => return None,
    };
    if awaited.kind() == "try_expression" {
        awaited = children(awaited).find(|part| !is_comment(*part))?;
    }
    let call = children(awaited).find(|part| !is_comment(*part))?;
    let callee = call.child_by_field_name("function")?;
    // A zero-argument named call cannot take ownership of the guard.
    (awaited.kind() == "await_expression"
        && call.kind() == "call_expression"
        && matches!(callee.kind(), "identifier" | "scoped_identifier")
        && arguments(call).next().is_none())
    .then_some(awaited)
}

fn wall_clock_unwrapped<'tree>(source: &Source<'tree, '_>, node: Node<'tree>) -> bool {
    let Some((elapsed, unwrap)) = method_call(node) else {
        return false;
    };
    if !matches!(source.text(unwrap), "unwrap" | "expect") {
        return false;
    }
    let Some((timestamp, method)) = method_call(elapsed) else {
        return false;
    };
    if source.text(method) != "elapsed" || arguments(elapsed).next().is_some() {
        return false;
    }
    let start = match timestamp.kind() {
        "call_expression" => Some(timestamp),
        "identifier" => source
            .local_binding(timestamp)
            .filter(|binding| !children(*binding).any(|node| node.kind() == "mutable_specifier"))
            .and_then(|binding| binding.child_by_field_name("value")),
        _ => None,
    };
    start.is_some_and(|start| {
        call_path(source, start).as_deref() == Some("std::time::SystemTime::now")
    })
}

fn function_owner(node: Node<'_>) -> Option<Node<'_>> {
    ancestors(node)
        .find(|ancestor| is_execution_boundary(*ancestor))
        .filter(|ancestor| ancestor.kind() == "function_item")
}

fn attr_name<'tree>(source: &Source<'tree, '_>, attr: Node<'tree>) -> Option<String> {
    let name = children(attr).next()?;
    if name.kind() == "identifier" && matches!(source.text(name), "test" | "should_panic") {
        return Some(source.text(name).to_owned());
    }
    source.external_path(name)
}

fn blocks_current_thread<'tree>(source: &Source<'tree, '_>, node: Node<'tree>) -> bool {
    if call_path(source, node).as_deref() != Some("tokio::task::block_in_place") {
        return false;
    }
    let Some(owner) = function_owner(node) else {
        return false;
    };
    if attributes(owner).any(|attr| attr_name(source, attr).as_deref() == Some("should_panic")) {
        return false;
    }
    attributes(owner).any(|attr| {
        matches!(
            attr_name(source, attr).as_deref(),
            Some("tokio::test" | "tokio::main")
        ) && children(attr)
            .any(|node| node.kind() == "token_tree" && explicit_current_thread(source, node))
    })
}

fn explicit_current_thread(source: &Source<'_, '_>, args: Node<'_>) -> bool {
    let mut cursor = args.walk();
    let tokens: Vec<_> = args
        .children(&mut cursor)
        .filter(|node| !is_comment(*node))
        .collect();
    tokens.windows(3).any(|parts| {
        parts[0].kind() == "identifier"
            && source.text(parts[0]) == "flavor"
            && parts[1].kind() == "="
            && matches!(
                source.text(parts[2]),
                "\"current_thread\"" | "r\"current_thread\""
            )
    })
}

fn discards_assertion_task<'tree>(
    source: &Source<'tree, '_>,
    statement: Node<'tree>,
    joined: bool,
) -> bool {
    let call = if joined {
        let wildcard = statement.child_by_field_name("pattern");
        if !wildcard.is_some_and(|pattern| source.text(pattern) == "_") {
            return false;
        }
        statement
            .child_by_field_name("value")
            .filter(|value| value.kind() == "await_expression")
            .and_then(|awaited| children(awaited).find(|node| !is_comment(*node)))
    } else {
        children(statement).find(|node| !is_comment(*node))
    };
    let Some(call) = call else { return false };
    if call_path(source, call).as_deref() != Some("tokio::spawn") {
        return false;
    }
    if !test_body(source, statement) {
        return false;
    }
    arguments(call).next().is_some_and(|future| {
        future.kind() == "async_block"
            && descendants(future).any(|part| {
                part.kind() == "macro_invocation"
                    && part.child_by_field_name("macro").is_some_and(|name| {
                        source.standard_macro(name, &["assert", "assert_eq", "assert_ne"])
                    })
                    && ancestors(part).find(|parent| is_execution_boundary(*parent)) == Some(future)
            })
    })
}

pub(super) fn call_path<'tree>(source: &Source<'tree, '_>, node: Node<'tree>) -> Option<String> {
    if node.kind() != "call_expression" {
        return None;
    }
    source.external_path(node.child_by_field_name("function")?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn timeout_tracks_owned_join_handles_from_local_spawns() {
        let source = r#"
use tokio::spawn as launch;
use tokio::time::timeout as bounded;

async fn work() {
    let task = launch(job());
    let _ = bounded(deadline(), task).await;
    let mut other = launch(job());
    let _ = bounded(deadline(), &mut other).await;
    other.abort();
    let ordinary = job();
    let _ = bounded(deadline(), ordinary).await;
    let shadow = launch(job());
    let shadow = job();
    let _ = bounded(deadline(), shadow).await;
}
"#;
        let found = super::super::analyze_file("tasks.rs", source);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].source.rule_id.as_deref(), Some("rust-timeout-consumes-join-handle"));
        assert_eq!(found[0].related_locations.as_ref().unwrap()[0].snippet, "let task = launch(job());");
    }
    use super::super::analyze_file;

    fn ids(source: &str) -> Vec<String> {
        analyze_file("lib.rs", source)
            .into_iter()
            .filter_map(|f| f.source.rule_id)
            .collect()
    }

    #[test]
    fn only_standard_matches_in_executed_test_bodies_is_reported() {
        assert_eq!(
            ids(r#"
use std::matches as fits;
#[test]
fn example() {
    matches!(result(), Ok(_));
    fits!(result(), Err(_));
    assert!(matches!(result(), Ok(_)));
    let _callback = || { matches!(result(), Ok(_)); };
}
#[test]
#[should_panic]
fn checks_panic() { matches!(panic!("expected"), ()); }
"#),
            ["rust-discarded-test-matches", "rust-discarded-test-matches"]
        );
        assert!(ids(r#"
macro_rules! matches { ($($t:tt)*) => { () }; }
macro_rules! assert { ($($t:tt)*) => { () }; }
#[tokio::test]
async fn example() {
    matches!(result(), Ok(_));
    tokio::spawn(async { assert!(true); });
}
"#)
        .is_empty());
    }

    #[test]
    fn generic_and_let_chain_bindings_shadow_external_names() {
        assert!(ids(r#"
use std::time::SystemTime as Clock;
use tokio::task::block_in_place as blocking;
fn with_clock<Clock: LocalClock>() {
    Clock::now().elapsed().unwrap();
}
#[tokio::test(flavor = "current_thread")]
async fn let_chain() {
    if let Some(blocking) = local() && { blocking(|| ()); true } {}
}
"#)
        .is_empty());
    }

    #[test]
    fn finds_import_aliases_and_tracks_immutable_timers() {
        assert_eq!(
            ids(r#"
use std::time::{self as clock, SystemTime as Wall};
fn measure() {
    let start = Wall::now();
    work();
    start.elapsed().unwrap();
    clock::SystemTime::now().elapsed().expect("clock");
    let mut start = Wall::now();
    start = another_clock();
    start.elapsed().unwrap();
    {
        let start = timer();
        start.elapsed().unwrap();
    }
}
"#),
            [
                "rust-wall-clock-elapsed-unwrap",
                "rust-wall-clock-elapsed-unwrap"
            ]
        );
    }

    #[test]
    fn respects_module_local_names_and_shadowing() {
        assert_eq!(
            ids(r#"
use std::time::SystemTime as Wall;
mod child {
    fn run() { Wall::now().elapsed().unwrap(); }
}
fn run(Wall: Clock) {
    Wall::now().elapsed().unwrap();
}
mod std {}
fn aliases() {
    use std::time::SystemTime as Clock;
    Clock::now().elapsed().unwrap();
}
"#),
            Vec::<String>::new()
        );
    }

    #[test]
    fn finds_discarded_assertions_and_only_explicit_current_thread() {
        assert_eq!(
            ids(r#"
use tokio::{spawn as launch, task::block_in_place as blocking};
#[tokio::test(flavor = "current_thread")]
async fn execute() {
    launch(async { assert_eq!(value(), 1); });
    let _ = launch(async { assert!(value() > 0); }).await;
    launch(async { let task = || assert_eq!(value(), 1); });
    launch(async { assert_eq!(value(), 1); }).await.unwrap();
    blocking(|| ());
    let run = || blocking(|| ());
}
#[tokio::test(name = "current_thread", flavor = "multi_thread")]
async fn compatible_runtime() { blocking(|| ()); }
#[tokio::test(flavor = "current_thread")]
#[should_panic]
async fn exercises_the_panic() { blocking(|| ()); }
"#),
            [
                "rust-detached-test-assertion",
                "rust-discarded-test-join-error",
                "rust-block-in-place-current-thread"
            ]
        );
    }

    #[test]
    fn traces_named_span_guard_across_an_adjacent_await() {
        assert_eq!(
            ids(r#"
use tracing::info_span as span;
async fn execute() -> Result<(), Error> {
    let work = span!("execute");
    let _guard = work.enter();
    dispatch().await?;
    Ok(())
}
async fn released() {
    let guard = tracing::info_span!("init").entered();
    drop(guard);
    dispatch().await;
}
async fn scoped() {
    { let _guard = tracing::info_span!("init").entered(); }
    dispatch().await;
}
"#),
            ["rust-tracing-guard-across-await"]
        );
    }
}
