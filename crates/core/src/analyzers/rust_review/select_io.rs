use autoreview_schema::Severity;
use tree_sitter::Node;

use super::syntax::{ancestors, children, is_comment, Source};
use super::{Review, RustReviewRule};

pub(super) const RULE: RustReviewRule = RustReviewRule {
    id: "rust-tokio-select-restarts-read-exact",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Tokio select loop can restart a partially completed read",
    message: "A competing branch can drop this read_exact future after it consumes bytes. Retain a pinned read across iterations or read into a buffer that tracks progress.",
    reference: "https://docs.rs/tokio/latest/tokio/macro.select.html#cancellation-safety",
    cwe: None,
};

pub(super) fn check<'t>(review: &mut Review<'t, '_>, node: Node<'t>) {
    let Some(tree) = select_tokens(&review.source, node) else { return };
    let Some(loop_node) = containing_loop(node) else { return };
    for method in read_exact_arms(&review.source, tree) {
        review.report_related(&RULE, method, &[loop_node]);
    }
}

fn select_tokens<'t>(source: &Source<'t, '_>, node: Node<'t>) -> Option<Node<'t>> {
    if node.kind() != "macro_invocation" {
        return None;
    }
    let mac = node.child_by_field_name("macro")?;
    if source.external_path(mac).as_deref() != Some("tokio::select") {
        return None;
    }
    children(node).find(|child| child.kind() == "token_tree")
}

fn read_exact_arms<'t>(source: &Source<'t, '_>, tree: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = tree.walk();
    let tokens: Vec<_> = tree.children(&mut cursor).filter(|child| !is_comment(*child)).collect();
    if tokens.iter().filter(|token| token.kind() == "=>").count() < 2 {
        return Vec::new();
    }
    tokens.windows(6).filter_map(|arm| read_exact_arm(source, arm)).collect()
}

fn read_exact_arm<'t>(source: &Source<'t, '_>, arm: &[Node<'t>]) -> Option<Node<'t>> {
    let [equals, reader, dot, method, arguments, arrow] = arm else { return None };
    (equals.kind() == "=" && reader.kind() == "identifier" && dot.kind() == "."
        && source.text(*method) == "read_exact" && arguments.kind() == "token_tree"
        && arrow.kind() == "=>" && known_reader(source, *reader)).then_some(*method)
}

fn containing_loop(node: Node<'_>) -> Option<Node<'_>> {
    ancestors(node).find(|parent| matches!(
        parent.kind(), "loop_expression" | "while_expression" | "for_expression"
            | "function_item" | "closure_expression" | "async_block"
    )).filter(|parent| matches!(parent.kind(), "loop_expression" | "while_expression" | "for_expression"))
}

fn known_reader<'t>(source: &Source<'t, '_>, reader: Node<'t>) -> bool {
    let Some(mut ty) = source.declaration(reader).and_then(|binding| binding.child_by_field_name("type")) else {
        return false;
    };
    while matches!(ty.kind(), "reference_type" | "generic_type") {
        let Some(inner) = ty.child_by_field_name("type") else { return false };
        ty = inner;
    }
    matches!(source.external_path(ty).as_deref(),
        Some("tokio::net::TcpStream" | "tokio::net::UnixStream" | "tokio::fs::File"
            | "tokio::io::DuplexStream" | "tokio::io::BufReader" | "tokio::io::BufStream"))
}

#[cfg(test)]
mod tests {
    use super::super::analyze_file;

    #[test]
    fn flags_direct_reads_but_allows_retained_futures_and_read() {
        let source = r#"
use tokio::{select as race, io::AsyncReadExt, net::TcpStream};

async fn restarted(mut stream: TcpStream, buf: &mut [u8]) {
    loop {
        race! {
            result = stream.read_exact(buf) => { handle(result); }
            _ = tick() => { continue; }
        }
    }
}

async fn retained(mut stream: TcpStream, buf: &mut [u8]) {
    let read = stream.read_exact(buf);
    tokio::pin!(read);
    loop {
        tokio::select! {
            _ = &mut read => { break; }
            _ = tick() => {}
        }
    }
}

async fn safe(mut stream: TcpStream, buf: &mut [u8]) {
    loop {
        tokio::select! {
            _ = stream.read(buf) => {}
            _ = tick() => {}
        }
    }
}

async fn unrelated(mut reader: Reader, buf: &mut [u8]) {
    loop {
        race! {
            _ = reader.read_exact(buf) => {}
            _ = tick() => {}
        }
    }
}
"#;
        let found = analyze_file("read.rs", source);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].source.rule_id.as_deref(), Some(super::RULE.id));
        assert_eq!(found[0].location.snippet, "read_exact");
    }
}
