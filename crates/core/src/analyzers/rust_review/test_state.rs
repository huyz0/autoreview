use autoreview_schema::Severity;
use tree_sitter::Node;

use super::basics::test_body;
use super::syntax::{children, is_comment, Source};
use super::{Review, RustReviewRule};

const CURRENT_DIR: RustReviewRule = RustReviewRule {
    id: "rust-test-mutates-current-dir",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Test changes the process working directory",
    message: "Other tests in this process can observe the directory change, even if this test restores it. Use absolute paths for file operations or set the working directory on a child process with Command::current_dir.",
    reference: "https://doc.rust-lang.org/std/env/fn.set_current_dir.html",
    cwe: None,
};

const SELF_ASSERTION: RustReviewRule = RustReviewRule {
    id: "rust-tautological-assertion",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Test assertion compares a variable with itself",
    message: "This assertion compares the same variable on both sides. Assert against an independent expected value so the test can detect incorrect behavior.",
    reference: "https://doc.rust-lang.org/std/macro.assert_eq.html",
    cwe: None,
};

pub(super) const RULES: &[&RustReviewRule] = &[&CURRENT_DIR, &SELF_ASSERTION];

pub(super) fn check<'tree>(review: &mut Review<'tree, '_>, node: Node<'tree>) {
    let rule = match node.kind() {
        "call_expression" if changes_test_current_dir(&review.source, node) => &CURRENT_DIR,
        "macro_invocation" if self_assertion(&review.source, node) => &SELF_ASSERTION,
        _ => return,
    };
    review.report(rule, node);
}

fn self_assertion<'tree>(source: &Source<'tree, '_>, node: Node<'tree>) -> bool {
    node.child_by_field_name("macro")
        .is_some_and(|name| source.standard_macro(name, &["assert_eq", "debug_assert_eq"]))
        && children(node)
            .find(|child| child.kind() == "token_tree")
            .is_some_and(|tokens| same_identifier_pair(source, tokens))
        && test_body(source, node)
}

fn same_identifier_pair(source: &Source<'_, '_>, tokens: Node<'_>) -> bool {
    let mut cursor = tokens.walk();
    let mut parts = tokens
        .children(&mut cursor)
        .filter(|part| !is_comment(*part))
        .skip(1);
    let pair: Vec<_> = parts.by_ref().take(4).collect();
    matches!(pair.as_slice(), [left, comma, right, end]
        if left.kind() == "identifier" && comma.kind() == "," && right.kind() == "identifier"
            && matches!(end.kind(), "," | ")" | "]" | "}") && source.text(*left) == source.text(*right))
}

fn changes_test_current_dir<'tree>(source: &Source<'tree, '_>, call: Node<'tree>) -> bool {
    call.child_by_field_name("function")
        .and_then(|function| source.external_path(function))
        .is_some_and(|name| name == "std::env::set_current_dir")
        && test_body(source, call)
}

#[cfg(test)]
mod tests {
    use super::super::analyze_file;

    #[test]
    fn detects_repeated_bindings_in_standard_equality_assertions() {
        let sample = r#"
use std::assert_eq as same;
#[test]
fn checks() {
    let actual = source();
    same!(actual, actual, "result");
    assert_eq!(actual, 42);
    assert_eq!(source(), source());
    assert_eq!(actual, actual + 1);
    macro_rules! debug_assert_eq { ($a:expr, $b:expr) => {}; }
    debug_assert_eq!(actual, actual);
}
fn program() { assert_eq!(value, value); }
"#;
        let found = analyze_file("lib.rs", sample);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(
            found[0].source.rule_id.as_deref(),
            Some("rust-tautological-assertion")
        );
    }

    #[test]
    fn flags_process_directory_changes_in_test_bodies() {
        let sample = r#"
use std::env::set_current_dir as switch_dir;

#[test]
fn changes_process_directory() {
    switch_dir("fixtures").unwrap();
}

#[test]
fn uses_absolute_paths_and_child_working_directory() {
    let path = std::env::current_dir().unwrap().join("fixtures");
    let _child = std::process::Command::new("example").current_dir(path);
}

fn application_entrypoint() {
    std::env::set_current_dir("fixtures").unwrap();
}

#[test]
fn uses_unrelated_function() {
    let switch_dir = |_| Ok::<(), ()>(());
    switch_dir("fixtures").unwrap();
}
"#;
        let findings: Vec<_> = analyze_file("lib.rs", sample)
            .into_iter()
            .filter_map(|finding| finding.source.rule_id)
            .collect();
        assert_eq!(findings, ["rust-test-mutates-current-dir"]);
    }
}
