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

use autoreview_langsupport::Language;
use tree_sitter::Node;

use crate::cfg::{Cfg, NodeId, Stmt};

/// Lower control flow used exclusively by keyed call-sequence rules. Use
/// the ordinary graph when the function has no deferred or finally path.
pub fn keyed_variant(language: Language, source: &[u8], function: Node<'_>) -> Option<Cfg<Stmt>> {
    let lower = match language {
        Language::Go => go::lower_with_cfg,
        Language::Java => java::lower_with_cfg,
        Language::Kotlin => kotlin::lower_with_cfg,
        Language::JavaScript | Language::TypeScript | Language::Tsx => javascript::lower_with_cfg,
        _ => return None,
    };
    keyed_flow::needed(function).then(|| {
        let mut cfg = Cfg::new_empty();
        cfg.keyed_flow = Some(128);
        lower(cfg, source, function)
    })
}

type StatementLowerer = fn(&mut Cfg<Stmt>, Node<'_>, &[u8], NodeId) -> NodeId;

fn lower_block(cfg: &mut Cfg<Stmt>, block: Node<'_>, source: &[u8], mut current: NodeId, lower: StatementLowerer) -> NodeId {
    let mut cursor = block.walk();
    for stmt in block.named_children(&mut cursor) {
        current = lower_statement(cfg, stmt, source, current, lower);
    }
    current
}

fn lower_statement(cfg: &mut Cfg<Stmt>, stmt: Node<'_>, source: &[u8], current: NodeId, lower: StatementLowerer) -> NodeId {
    let start = cfg.nodes[current].stmts.len();
    let next = if cfg.keyed_flow.is_some() {
        keyed_flow::lower(cfg, stmt, source, current, lower)
    } else {
        lower(cfg, stmt, source, current)
    };
    cfg.nodes[current].record_statement_lines(start, stmt.start_position().row as u32 + 1);
    next
}
