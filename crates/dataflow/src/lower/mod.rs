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

use crate::cfg::{Cfg, EdgeKind, NodeId, Stmt};

/// Lower control flow used exclusively by keyed call-sequence rules.
pub fn keyed_variant(language: Language, source: &[u8], node: Node<'_>) -> Cfg<Stmt> {
    let lower: StatementLowerer = match language {
        Language::Go => go::lower_block,
        Language::Java => java::lower_block,
        Language::Kotlin => kotlin::lower_block,
        Language::JavaScript | Language::TypeScript | Language::Tsx => javascript::lower_block,
        Language::Rust => return rust::lower_keyed(source, node),
    };
    let mut cfg = Cfg::new_empty();
    cfg.keyed_flow = Some(keyed_flow::EXIT_PATH_LIMIT);
    cfg.keyed_exceptions = language != Language::Go;
    let body = if language == Language::Kotlin { kotlin::function_body(node) } else { node.child_by_field_name("body") };
    function(cfg, source, node, body, lower)
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
    cfg.nodes[current].record_statement_lines(start, stmt.start_position().row as u32 + 1);
    next
}
