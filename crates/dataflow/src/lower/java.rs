//! Java CST → `Cfg` lowering. Grammar shapes verified empirically via
//! `ast-grep --debug-query=ast` against `tree-sitter-java` — meaningfully
//! different from Go's grammar in several places worth calling out
//! explicitly rather than assuming parity:
//! - A `block`'s statements are *direct* named children (no
//!   `statement_list` wrapper the way Go's `block` has one).
//! - `if_statement`'s `consequence`/`alternative` are named fields
//!   directly on the node (Go's equivalent needs the same field names,
//!   but Java's `alternative` can be either a `block` or another bare
//!   statement for an `else if` chain — handled the same way Go's
//!   `lower_if` already does for that case).
//! - `return_statement`'s value is a *direct* child (an `identifier`),
//!   not nested inside an `expression_list` the way Go's is — this
//!   project's own Go lowering had a bug here in an earlier phase from
//!   assuming Go's shape without checking; verified separately for Java
//!   here rather than reusing that assumption.
//! - Java has no `:=` — all assignment is `assignment_expression` inside
//!   an `expression_statement`, and local declarations are
//!   `local_variable_declaration -> declarator: variable_declarator ->
//!   name`/`value`.
//! - `null` is its own grammar node kind (`null_literal`), matching Go's
//!   `nil` being its own kind rather than a regular identifier.
//!
//! Scope: no dataflow rule targets Java yet (Phase 6 is architectural
//! completion of the CFG core across all three languages, not new rule
//! surface) — this module covers the same statement shapes Go's does
//! (assignment, return, if/else, for, while) so the generic
//! `cfg`/`lattice`/`solver` core is proven to generalize, verified via
//! this module's own lowering tests rather than an end-to-end rule.

use tree_sitter::Node;

use crate::cfg::{Cfg, EdgeKind, NodeId, RhsShape, Stmt};

fn text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("").trim()
}

fn classify_rhs(node: Node, source: &[u8]) -> RhsShape {
    match node.kind() {
        "null_literal" => RhsShape::NilLiteral,
        "identifier" => RhsShape::Var(text(node, source).to_string()),
        _ => RhsShape::Unknown,
    }
}

fn call_arg_identifiers(call: Node, source: &[u8]) -> Vec<String> {
    call.child_by_field_name("arguments")
        .map(|args| {
            let mut cursor = args.walk();
            args.named_children(&mut cursor).filter(|n| n.kind() == "identifier").map(|n| text(n, source).to_string()).collect()
        })
        .unwrap_or_default()
}

/// Lowers `local_variable_declaration` (`Type x = expr;` or `Type x;`).
/// Only the single-declarator case is modeled, same precision-over-
/// generality tradeoff as the Go lowering.
fn lower_local_variable_declaration(node: Node, source: &[u8]) -> Stmt {
    let mut cursor = node.walk();
    let declarators: Vec<Node> = node.named_children(&mut cursor).filter(|n| n.kind() == "variable_declarator").collect();
    if declarators.len() != 1 {
        return Stmt::Other(text(node, source).to_string());
    }
    let Some(name_node) = declarators[0].child_by_field_name("name") else { return Stmt::Other(text(node, source).to_string()) };
    let lhs = text(name_node, source).to_string();
    match declarators[0].child_by_field_name("value") {
        Some(value) if value.kind() == "method_invocation" => match value.child_by_field_name("name") {
            Some(name) => Stmt::Call { target: crate::cfg::CallTarget::Named(text(name, source).to_string()), args: call_arg_identifiers(value, source), assigned_to: Some(lhs) },
            None => Stmt::Assign { lhs, rhs: RhsShape::Unknown },
        },
        Some(value) => Stmt::Assign { lhs, rhs: classify_rhs(value, source) },
        // No initializer: Java zero-initializes (0/false/null depending
        // on type), but this lowering doesn't currently track a
        // variable's declared type well enough to distinguish those —
        // left as `Unknown` rather than guessing `NilLiteral` the way
        // Go's `var x *T` case safely can (Go's shape unambiguously
        // means "starts nil"; Java's doesn't without type resolution).
        None => Stmt::Assign { lhs, rhs: RhsShape::Unknown },
    }
}

/// Lowers `x = expr;` (`expression_statement -> assignment_expression`).
fn lower_assignment_expression(node: Node, source: &[u8]) -> Stmt {
    let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else {
        return Stmt::Other(text(node, source).to_string());
    };
    if left.kind() != "identifier" {
        return Stmt::Other(text(node, source).to_string());
    }
    let lhs = text(left, source).to_string();
    if right.kind() == "method_invocation" {
        if let Some(name_node) = right.child_by_field_name("name") {
            return Stmt::Call { target: crate::cfg::CallTarget::Named(text(name_node, source).to_string()), args: call_arg_identifiers(right, source), assigned_to: Some(lhs) };
        }
    }
    Stmt::Assign { lhs, rhs: classify_rhs(right, source) }
}

/// Lowers one method/constructor body into a `Cfg`. `fn_node` must be a
/// `method_declaration` or `constructor_declaration` with a `body` field.
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

/// Lowers a `block`'s statements, which — unlike Go's `block` — are
/// direct named children with no `statement_list` wrapper.
fn lower_block(cfg: &mut Cfg<Stmt>, block: Node, source: &[u8], mut current: NodeId) -> NodeId {
    let mut cursor = block.walk();
    for stmt in block.named_children(&mut cursor) {
        current = lower_statement(cfg, stmt, source, current);
    }
    current
}

fn lower_statement(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    match stmt.kind() {
        "local_variable_declaration" => {
            cfg.nodes[current].stmts.push(lower_local_variable_declaration(stmt, source));
            current
        }
        "expression_statement" => {
            if let Some(inner) = stmt.named_child(0) {
                match inner.kind() {
                    "assignment_expression" => {
                        cfg.nodes[current].stmts.push(lower_assignment_expression(inner, source));
                        return current;
                    }
                    // A bare call statement (`stmt.executeUpdate(sql);`, no
                    // assignment) — without this, a sink whose return value
                    // is discarded (the common case for e.g. `Statement`'s
                    // mutating methods) would be invisible to the taint
                    // engine, which only inspects `Stmt::Call` nodes.
                    "method_invocation" => {
                        if let Some(name) = inner.child_by_field_name("name") {
                            cfg.nodes[current].stmts.push(Stmt::Call { target: crate::cfg::CallTarget::Named(text(name, source).to_string()), args: call_arg_identifiers(inner, source), assigned_to: None });
                            return current;
                        }
                    }
                    _ => {}
                }
            }
            cfg.nodes[current].stmts.push(Stmt::Other(text(stmt, source).to_string()));
            current
        }
        "return_statement" => {
            // Unlike Go, `return_statement`'s value is a direct child
            // (verified separately, not assumed from Go's shape).
            let value = stmt.named_child(0).filter(|n| n.kind() == "identifier").map(|n| text(n, source).to_string());
            cfg.nodes[current].stmts.push(Stmt::Return { value });
            cfg.edges.push((current, cfg.exit, EdgeKind::Fallthrough));
            cfg.push_node(stmt.start_position().row as u32 + 1)
        }
        "if_statement" => lower_if(cfg, stmt, source, current),
        "for_statement" | "while_statement" | "enhanced_for_statement" => lower_loop(cfg, stmt, source, current),
        _ => {
            cfg.nodes[current].stmts.push(Stmt::Other(text(stmt, source).to_string()));
            current
        }
    }
}

fn lower_if(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    if let Some(cond) = stmt.child_by_field_name("condition") {
        cfg.nodes[current].stmts.push(Stmt::Other(format!("if {}", text(cond, source))));
    }

    let true_start = cfg.push_node(stmt.start_position().row as u32 + 1);
    cfg.edges.push((current, true_start, EdgeKind::True));
    let true_end = match stmt.child_by_field_name("consequence") {
        Some(block) if block.kind() == "block" => lower_block(cfg, block, source, true_start),
        Some(single) => lower_statement(cfg, single, source, true_start),
        None => true_start,
    };

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
        cfg.edges.push((current, join, EdgeKind::False));
    }
    join
}

/// Shared lowering for `for`/`while`/`for-each` — all three have a
/// `body` field and the same loop-with-back-edge shape; only the
/// header's own init/condition/update content differs, none of which any
/// current rule needs modeled.
fn lower_loop(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
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
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Java).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        fn find_method(node: Node) -> Option<Node> {
            if node.kind() == "method_declaration" {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = find_method(child) {
                    return Some(found);
                }
            }
            None
        }
        let fn_node = find_method(root).expect("no method_declaration found");
        lower_function(source.as_bytes(), fn_node)
    }

    #[test]
    fn lowers_a_straight_line_method() {
        let cfg = lower("class Foo {\n    int f() {\n        int x = 1;\n        return x;\n    }\n}\n");
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Assign { lhs, .. } if lhs == "x"))));
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Return { value: Some(v) } if v == "x"))));
    }

    #[test]
    fn lowers_an_if_else_into_true_false_edges() {
        let cfg = lower("class Foo {\n    void f(int x) {\n        int y = 0;\n        if (x > 0) {\n            y = 1;\n        } else {\n            y = 2;\n        }\n    }\n}\n");
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::True));
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::False));
    }

    #[test]
    fn lowers_a_null_assignment() {
        let cfg = lower("class Foo {\n    Object f() {\n        Object o = null;\n        return o;\n    }\n}\n");
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Assign { lhs, rhs: RhsShape::NilLiteral } if lhs == "o"))), "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn lowers_a_for_loop_with_a_back_edge() {
        let cfg = lower("class Foo {\n    void f() {\n        for (int i = 0; i < 10; i++) {\n            int y = i;\n        }\n    }\n}\n");
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::Loop));
    }

    #[test]
    fn lowers_a_while_loop_with_a_back_edge() {
        let cfg = lower("class Foo {\n    void f() {\n        while (true) {\n            int y = 1;\n        }\n    }\n}\n");
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::Loop));
    }

    #[test]
    fn recognizes_a_bare_method_call_statement_with_no_assignment() {
        let cfg = lower("class Foo {\n    void f(String sql) {\n        stmt.executeUpdate(sql);\n    }\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), assigned_to: None, args } if name == "executeUpdate" && args.contains(&"sql".to_string())))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_a_method_call_assigned_to_a_variable() {
        let cfg = lower("class Foo {\n    Object f() {\n        Object e = helper();\n        return e;\n    }\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), assigned_to: Some(a), .. } if name == "helper" && a == "e"))),
            "got: {:#?}",
            cfg.nodes
        );
    }
}
