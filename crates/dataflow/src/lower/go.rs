//! Go CST → `Cfg` lowering. Grammar shapes below verified empirically via
//! `ast-grep --debug-query=ast` against `tree-sitter-go` (its bundled
//! grammar and the `tree-sitter-go` crate agree on field names here,
//! unlike the Kotlin grammar-fork mismatch documented elsewhere in this
//! project) — not assumed from the grammar's own docs.
//!
//! Scope: only the statement shapes the current dataflow-powered rules
//! need are specifically recognized (`short_var_declaration`,
//! `assignment_statement`, `return_statement`, `if_statement`,
//! `for_statement`, and `append(...)` calls). Everything else lowers to
//! `Stmt::Other(text)` — present in the graph for control-flow shape and
//! textual fallback, not specially modeled.

use tree_sitter::Node;

use crate::cfg::{Cfg, EdgeKind, NodeId, RhsShape, Stmt};

fn text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("").trim()
}

/// Classifies a single right-hand-side expression node into the coarse
/// `RhsShape` the dataflow rules care about.
fn classify_rhs(node: Node, source: &[u8]) -> RhsShape {
    match node.kind() {
        "slice_expression" => {
            if let Some(operand) = node.child_by_field_name("operand") {
                RhsShape::Reslice { of: text(operand, source).to_string() }
            } else {
                RhsShape::Unknown
            }
        }
        "identifier" => {
            let t = text(node, source);
            if t == "nil" { RhsShape::NilLiteral } else { RhsShape::Var(t.to_string()) }
        }
        _ => RhsShape::Unknown,
    }
}

/// Lowers a `left := right` / `left = right` shaped statement (both
/// `short_var_declaration` and `assignment_statement` use the same
/// `left`/`right` field names in this grammar) into a `Stmt`, handling
/// the `x = append(x, ...)` special case as a `Stmt::Call` rather than a
/// generic assign, since the rules need to see it as a call.
fn lower_assign_like(node: Node, source: &[u8]) -> Stmt {
    let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else {
        return Stmt::Other(text(node, source).to_string());
    };
    // Only the single-target, single-value case is modeled — tuple
    // assignment (`a, b := f()`) is common for Go's `(value, err)`
    // pattern but isn't needed by any current rule, so it falls back to
    // `Other` rather than guessing which side is which.
    if left.named_child_count() != 1 || right.named_child_count() != 1 {
        return Stmt::Other(text(node, source).to_string());
    }
    let lhs_node = left.named_child(0).unwrap();
    if lhs_node.kind() != "identifier" {
        return Stmt::Other(text(node, source).to_string());
    }
    let lhs = text(lhs_node, source).to_string();
    let rhs_node = right.named_child(0).unwrap();

    if rhs_node.kind() == "call_expression" {
        if let Some(func) = rhs_node.child_by_field_name("function") {
            if text(func, source) == "append" {
                let args = rhs_node
                    .child_by_field_name("arguments")
                    .map(|args| {
                        let mut cursor = args.walk();
                        args.named_children(&mut cursor).filter(|n| n.kind() == "identifier").map(|n| text(n, source).to_string()).collect()
                    })
                    .unwrap_or_default();
                return Stmt::Call { target: crate::cfg::CallTarget::Named("append".to_string()), args, assigned_to: Some(lhs) };
            }
        }
    }

    Stmt::Assign { lhs, rhs: classify_rhs(rhs_node, source) }
}

/// Lowers one function/method body into a `Cfg`. `fn_node` must be a
/// `function_declaration` or `method_declaration` with a `body` field.
pub fn lower_function(source: &[u8], fn_node: Node) -> Cfg<Stmt> {
    let mut cfg: Cfg<Stmt> = Cfg::new_empty();
    let entry = cfg.push_node(fn_node.start_position().row as u32 + 1);
    let exit = cfg.push_node(fn_node.end_position().row as u32 + 1);
    cfg.entry = entry;
    cfg.exit = exit;

    let Some(body) = fn_node.child_by_field_name("body") else {
        cfg.edges.push((entry, exit, EdgeKind::Fallthrough));
        return cfg;
    };

    let end = lower_block(&mut cfg, body, source, entry);
    cfg.edges.push((end, exit, EdgeKind::Fallthrough));
    cfg
}

/// Lowers a `block` node's `statement_list`, threading the "current open
/// node" through sequentially and returning the node execution falls
/// through to after the block. `current`'s stmts get appended to directly
/// for straight-line statements; control-flow statements close it out and
/// open fresh nodes for their branches/loop bodies.
fn lower_block(cfg: &mut Cfg<Stmt>, block: Node, source: &[u8], mut current: NodeId) -> NodeId {
    // `statement_list` is a positional child of `block`, not a named
    // field (confirmed via `--debug-query=ast`: it has no `field:`
    // prefix in the dump) — find it by kind rather than
    // `child_by_field_name`. An empty block has no `statement_list` child
    // at all.
    let mut cursor = block.walk();
    let list = block.named_children(&mut cursor).find(|n| n.kind() == "statement_list");
    let Some(list) = list else { return current };

    let mut cursor = list.walk();
    for stmt in list.named_children(&mut cursor) {
        current = lower_statement(cfg, stmt, source, current);
    }
    current
}

fn lower_statement(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    match stmt.kind() {
        "short_var_declaration" | "assignment_statement" => {
            cfg.nodes[current].stmts.push(lower_assign_like(stmt, source));
            current
        }
        "return_statement" => {
            let value = stmt
                .child_by_field_name("value")
                .or_else(|| stmt.named_child(0))
                .filter(|n| n.kind() == "identifier")
                .map(|n| text(n, source).to_string());
            cfg.nodes[current].stmts.push(Stmt::Return { value });
            cfg.edges.push((current, cfg.exit, EdgeKind::Fallthrough));
            // Anything textually after a `return` in the same block is
            // unreachable fallthrough-wise; give it a fresh, disconnected
            // node so it doesn't corrupt `current`'s facts.
            cfg.push_node(stmt.start_position().row as u32 + 1)
        }
        "if_statement" => lower_if(cfg, stmt, source, current),
        "for_statement" => lower_for(cfg, stmt, source, current),
        "expression_statement" => {
            cfg.nodes[current].stmts.push(Stmt::Other(text(stmt, source).to_string()));
            current
        }
        _ => {
            cfg.nodes[current].stmts.push(Stmt::Other(text(stmt, source).to_string()));
            current
        }
    }
}

fn lower_if(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    // The condition expression itself isn't modeled as a `Stmt` yet (no
    // current rule needs branch-condition facts) — recorded as `Other`
    // text on the branch point for line-number/debug fidelity.
    if let Some(cond) = stmt.child_by_field_name("condition") {
        cfg.nodes[current].stmts.push(Stmt::Other(format!("if {}", text(cond, source))));
    }

    let true_start = cfg.push_node(stmt.start_position().row as u32 + 1);
    cfg.edges.push((current, true_start, EdgeKind::True));
    let true_end = if let Some(consequence) = stmt.child_by_field_name("consequence") { lower_block(cfg, consequence, source, true_start) } else { true_start };

    let false_end = if let Some(alt) = stmt.child_by_field_name("alternative") {
        let false_start = cfg.push_node(stmt.start_position().row as u32 + 1);
        cfg.edges.push((current, false_start, EdgeKind::False));
        if alt.kind() == "block" { lower_block(cfg, alt, source, false_start) } else { lower_statement(cfg, alt, source, false_start) }
    } else {
        current
    };

    let join = cfg.push_node(stmt.end_position().row as u32 + 1);
    cfg.edges.push((true_end, join, EdgeKind::Fallthrough));
    if false_end != current || stmt.child_by_field_name("alternative").is_some() {
        cfg.edges.push((false_end, join, EdgeKind::Fallthrough));
    } else {
        // No else branch — the false path skips straight from `current`
        // to `join` without a separate node.
        cfg.edges.push((current, join, EdgeKind::False));
    }
    join
}

fn lower_for(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    let header = current;
    let body_start = cfg.push_node(stmt.start_position().row as u32 + 1);
    cfg.edges.push((header, body_start, EdgeKind::True));
    let body_end = if let Some(body) = stmt.child_by_field_name("body") { lower_block(cfg, body, source, body_start) } else { body_start };
    cfg.edges.push((body_end, header, EdgeKind::Loop));

    let after = cfg.push_node(stmt.end_position().row as u32 + 1);
    cfg.edges.push((header, after, EdgeKind::False));
    after
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower(source: &str) -> Cfg<Stmt> {
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let fn_node = root.named_children(&mut cursor).find(|n| n.kind() == "function_declaration").expect("no function_declaration found");
        lower_function(source.as_bytes(), fn_node)
    }

    #[test]
    fn lowers_a_straight_line_function() {
        let cfg = lower("package p\nfunc f() {\n\tx := 1\n\treturn\n}\n");
        // entry -> (single straight-line node with the assign+return) -> exit
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Assign { lhs, .. } if lhs == "x"))));
        assert!(cfg.edges.iter().any(|(_, to, _)| *to == cfg.exit));
    }

    #[test]
    fn lowers_an_if_else_into_true_false_edges() {
        let cfg = lower("package p\nfunc f(x int) {\n\tif x > 0 {\n\t\ty := 1\n\t\t_ = y\n\t} else {\n\t\tz := 2\n\t\t_ = z\n\t}\n}\n");
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::True));
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::False));
    }

    #[test]
    fn recognizes_a_reslice_and_an_append_reassignment() {
        let cfg = lower("package p\nfunc f(full []int) []int {\n\tsub := full[2:4]\n\tsub = append(sub, 9)\n\treturn sub\n}\n");
        let reslice_found = cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Assign { lhs, rhs: RhsShape::Reslice { of } } if lhs == "sub" && of == "full")));
        let append_found = cfg.nodes.iter().any(|n| {
            n.stmts.iter().any(|s| matches!(s, Stmt::Call { assigned_to: Some(a), args, .. } if a == "sub" && args.contains(&"sub".to_string())))
        });
        assert!(reslice_found, "got: {:#?}", cfg.nodes);
        assert!(append_found, "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn lowers_a_for_loop_with_a_back_edge() {
        let cfg = lower("package p\nfunc f() {\n\tfor i := 0; i < 3; i++ {\n\t\tx := i\n\t\t_ = x\n\t}\n}\n");
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::Loop));
    }
}
