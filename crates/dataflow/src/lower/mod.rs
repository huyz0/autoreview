//! Per-language CST → `Cfg` lowering. `go.rs` (Phase 3) backs three
//! shipped rules; `java.rs` and `kotlin.rs` (Phases 6/7) were architectural
//! completion of the generic CFG core, since proven out by real taint
//! rules too (see `dataflow_check.rs`). `javascript.rs` is one module
//! covering both JavaScript and TypeScript — `tree-sitter-typescript`'s
//! grammar is built directly on `tree-sitter-javascript`'s, so the same
//! field-based node access works against source parsed with either.
//!
//! Each language module's entry point is called once per function/method
//! already discovered by `autoreview_symindex`'s existing method
//! enumeration — this crate deliberately does not re-walk the repo to
//! "find every function" a second time.

pub mod go;
pub mod java;
pub mod javascript;
pub mod kotlin;
pub mod rust;
pub(crate) mod site;
pub(crate) mod keyed_flow;
pub(crate) mod keyed_syntax;

use autoreview_langsupport::Language;
use tree_sitter::Node;

use crate::cfg::{Cfg, EdgeKind, Evaluation, NodeId, Stmt};

pub(crate) fn result_closure(language: Language, source: &[u8], node: Node<'_>) -> Option<Cfg<Stmt>> {
    let body = if node.kind() == "lambda_literal" { node } else { node.child_by_field_name("body")? };
    if matches!(body.kind(), "block" | "statement_block" | "lambda_literal") {
        return Some(if body == node {
            function(keyed_graph(language), source, node, Some(body), kotlin::lower_block)
        } else {
            keyed_variant(language, source, node)
        });
    }
    Some(expression_graph(language, source, node, body))
}

fn expression_graph(language: Language, source: &[u8], node: Node<'_>, expr: Node<'_>) -> Cfg<Stmt> {
    let mut cfg = keyed_graph(language);
    cfg.entry = cfg.push_node(line(node));
    cfg.exit = cfg.push_node(node.end_position().row as u32 + 1);
    let entry = cfg.entry;
    keyed_flow::expression_body(&mut cfg, expr, source, entry);
    cfg
}

fn keyed_graph(language: Language) -> Cfg<Stmt> {
    let mut cfg = Cfg::new_empty();
    cfg.keyed_flow = Some(keyed_flow::EXIT_PATH_LIMIT);
    cfg.keyed_exceptions = language != Language::Go && language != Language::Rust;
    cfg
}

/// Lower control flow used exclusively by keyed call-sequence rules.
pub fn keyed_variant(language: Language, source: &[u8], node: Node<'_>) -> Cfg<Stmt> {
    let lower: StatementLowerer = match language {
        Language::Go => go::lower_block,
        Language::Java => java::lower_block,
        Language::Kotlin => kotlin::lower_block,
        Language::JavaScript | Language::TypeScript | Language::Tsx => javascript::lower_block,
        Language::Rust => return rust::lower_keyed(source, node),
    };
    let body = if language == Language::Kotlin { kotlin::function_body(node) } else { node.child_by_field_name("body") };
    let expr = if node.kind() == "arrow_function" {
        body.filter(|body| body.kind() != "statement_block")
    } else if language == Language::Kotlin && body.is_none() {
        kotlin::expression_body(node)
    } else {
        None
    };
    if let Some(expr) = expr {
        return expression_graph(language, source, node, expr);
    }
    function(keyed_graph(language), source, node, body, lower)
}

type StatementLowerer = fn(&mut Cfg<Stmt>, Node<'_>, &[u8], NodeId) -> NodeId;

fn function(mut cfg: Cfg<Stmt>, source: &[u8], node: Node<'_>, body: Option<Node<'_>>, lower_body: StatementLowerer) -> Cfg<Stmt> {
    let entry = cfg.push_node(node.start_position().row as u32 + 1);
    cfg.entry = entry;
    cfg.exit = cfg.push_node(node.end_position().row as u32 + 1);
    let end = body.map_or(entry, |block| lower_body(&mut cfg, block, source, entry));
    cfg.edges.push((end, cfg.exit, EdgeKind::Fallthrough));
    cfg
}

fn lower_block(cfg: &mut Cfg<Stmt>, block: Node<'_>, source: &[u8], mut current: NodeId, lower: StatementLowerer) -> NodeId {
    let mut cursor = block.walk();
    for stmt in block.named_children(&mut cursor) {
        current = lower_statement(cfg, stmt, source, current, lower);
    }
    current
}

fn lower_statement(cfg: &mut Cfg<Stmt>, stmt: Node<'_>, source: &[u8], current: NodeId, lower: StatementLowerer) -> NodeId {
    if cfg.keyed_flow.is_some() {
        return keyed_flow::lower(cfg, stmt, source, current, lower);
    }
    let start = cfg.nodes[current].stmts.len();
    let next = lower(cfg, stmt, source, current);
    cfg.nodes[current].record_statement_lines(start, line(stmt));
    next
}

fn line(node: Node<'_>) -> u32 {
    node.start_position().row as u32 + 1
}

fn record_evaluation(cfg: &mut Cfg<Stmt>, at: NodeId, node: Node<'_>) {
    if cfg.keyed_flow.is_some() {
        cfg.nodes[at].evaluations.push(Evaluation { span: node.byte_range(), syntax_kind: node.kind_id(), returned: false });
    }
}
