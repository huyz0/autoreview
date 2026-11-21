use autoreview_schema::Severity;
use tree_sitter::Node;

use super::syntax::{arguments, children, is_comment, Source};
use super::{Review, RustReviewRule};

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

pub(super) const RULES: &[&RustReviewRule] = &[&OWNED_TASK_TIMEOUT];

pub(super) fn check<'tree>(review: &mut Review<'tree, '_>, node: Node<'tree>) {
    if node.kind() != "await_expression" {
        return;
    }
    if let Some(binding) = owned_task_timeout(&review.source, node) {
        review.report_related(&OWNED_TASK_TIMEOUT, node, &[binding]);
    }
}

fn owned_task_timeout<'tree>(
    source: &Source<'tree, '_>,
    await_expr: Node<'tree>,
) -> Option<Node<'tree>> {
    let call = children(await_expr).find(|node| !is_comment(*node))?;
    let timeout = source.external_path(call.child_by_field_name("function")?)?;
    if !matches!(
        timeout.as_str(),
        "tokio::time::timeout" | "tokio::time::timeout_at"
    ) {
        return None;
    }
    let binding = source.local_binding(arguments(call).nth(1)?)?;
    if children(binding).any(|node| node.kind() == "mutable_specifier") {
        return None;
    }
    let initializer = binding.child_by_field_name("value")?;
    let spawn = source.external_path(initializer.child_by_field_name("function")?)?;
    matches!(
        spawn.as_str(),
        "tokio::spawn" | "tokio::task::spawn" | "tokio::task::spawn_blocking"
    )
    .then_some(binding)
}

#[cfg(test)]
mod tests {
    use super::super::analyze_file;

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
        let found = analyze_file("tasks.rs", source);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(
            found[0].source.rule_id.as_deref(),
            Some("rust-timeout-consumes-join-handle")
        );
        assert_eq!(
            found[0].related_locations.as_ref().unwrap()[0].snippet,
            "let task = launch(job());"
        );
    }
}
