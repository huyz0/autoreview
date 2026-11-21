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

use tree_sitter::Node;

use crate::cfg::{Cfg, NodeId, Stmt};

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
    let next = lower(cfg, stmt, source, current);
    cfg.nodes[current].record_statement_lines(start, stmt.start_position().row as u32 + 1);
    next
}
