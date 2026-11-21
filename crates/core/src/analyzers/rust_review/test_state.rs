use autoreview_schema::Severity;
use tree_sitter::Node;

use super::basics::test_body;
use super::syntax::Source;
use super::{Review, RustReviewRule};

pub(super) const RULE: RustReviewRule = RustReviewRule {
    id: "rust-test-mutates-current-dir",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Test changes the process working directory",
    message: "Other tests in this process can observe the directory change, even if this test restores it. Use absolute paths for file operations or set the working directory on a child process with Command::current_dir.",
    reference: "https://doc.rust-lang.org/std/env/fn.set_current_dir.html",
    cwe: None,
};

pub(super) fn check<'tree>(review: &mut Review<'tree, '_>, node: Node<'tree>) {
    if node.kind() == "call_expression" && changes_test_current_dir(&review.source, node) {
        review.report(&RULE, node);
    }
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
