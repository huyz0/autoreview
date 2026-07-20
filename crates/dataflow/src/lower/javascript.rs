//! JavaScript/TypeScript CST → `Cfg` lowering. One module for both:
//! `tree-sitter-typescript`'s grammar is built directly on top of
//! `tree-sitter-javascript`'s (confirmed empirically — a TS-only construct
//! like a type annotation just adds an extra `type:` field alongside the
//! same `name`/`value` fields a plain JS `variable_declarator` already has,
//! rather than changing the shape this module reads), so field-based
//! access here works unchanged against source parsed with either grammar.
//! Grammar shapes verified against the real `tree_sitter_javascript`/
//! `tree_sitter_typescript` crates via a standalone dump, not assumed.
//!
//! Scope: only the statement shapes the current dataflow-powered rules
//! need (assignment, return, if/else, for/while, calls) — same precision-
//! over-generality tradeoff as `lower::go`/`java`/`kotlin`. Arrow
//! functions, class methods, and destructuring aren't modeled; only a
//! top-level `function_declaration`'s body is lowered.

use tree_sitter::Node;

use crate::cfg::{Cfg, EdgeKind, NodeId, RhsShape, Stmt};

fn text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("").trim()
}

fn classify_rhs(node: Node, source: &[u8]) -> RhsShape {
    match node.kind() {
        "null" => RhsShape::NilLiteral,
        "identifier" => RhsShape::Var(text(node, source).to_string()),
        _ => RhsShape::Unknown,
    }
}

/// Resolves a `call_expression`'s callee to a name usable for taint-rule
/// matching: a bare `f(...)` call lowers to `"f"`; a member call
/// (`recv.method(...)`, grammar shape `call_expression -> function: member_
/// expression -> object/property`) lowers to `"recv.method"`, matching Go's
/// own qualified-selector convention.
fn call_target_name(call: Node, source: &[u8]) -> Option<String> {
    let func = call.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => Some(text(func, source).to_string()),
        "member_expression" => {
            let object = func.child_by_field_name("object")?;
            let property = func.child_by_field_name("property")?;
            Some(format!("{}.{}", text(object, source), text(property, source)))
        }
        _ => None,
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

/// Lowers `var x = expr;`/`let x = expr;`/`const x = expr;`
/// (`variable_declaration`/`lexical_declaration`, both containing a
/// `variable_declarator` with the same `name`/`value` fields — a `type:`
/// field is present too under TypeScript but not read here). Only the
/// single-declarator case is modeled, same precision-over-generality
/// tradeoff as the other lowering modules.
fn lower_var_decl(node: Node, source: &[u8]) -> Stmt {
    let fallback = || Stmt::Other(text(node, source).to_string());
    let mut cursor = node.walk();
    let Some(declarator) = node.named_children(&mut cursor).find(|n| n.kind() == "variable_declarator") else { return fallback() };
    let Some(name_node) = declarator.child_by_field_name("name").filter(|n| n.kind() == "identifier") else { return fallback() };
    let lhs = text(name_node, source).to_string();

    let Some(value) = declarator.child_by_field_name("value") else { return fallback() };
    if value.kind() == "call_expression" {
        if let Some(name) = call_target_name(value, source) {
            return Stmt::Call { target: crate::cfg::CallTarget::Named(name), args: call_arg_identifiers(value, source), assigned_to: Some(lhs) };
        }
    }
    Stmt::Assign { lhs, rhs: classify_rhs(value, source) }
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
    if right.kind() == "call_expression" {
        if let Some(name) = call_target_name(right, source) {
            return Stmt::Call { target: crate::cfg::CallTarget::Named(name), args: call_arg_identifiers(right, source), assigned_to: Some(lhs) };
        }
    }
    Stmt::Assign { lhs, rhs: classify_rhs(right, source) }
}

/// Lowers one function body into a `Cfg`. `fn_node` must be a
/// `function_declaration` with a `body` field (a `statement_block`).
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

/// Lowers a `statement_block`'s statements — direct named children, no
/// wrapper the way Go's `block -> statement_list` has one.
fn lower_block(cfg: &mut Cfg<Stmt>, block: Node, source: &[u8], mut current: NodeId) -> NodeId {
    let mut cursor = block.walk();
    for stmt in block.named_children(&mut cursor) {
        current = lower_statement(cfg, stmt, source, current);
    }
    current
}

fn lower_statement(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    match stmt.kind() {
        "variable_declaration" | "lexical_declaration" => {
            cfg.nodes[current].stmts.push(lower_var_decl(stmt, source));
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
                    // is discarded would be invisible to the taint engine,
                    // which only inspects `Stmt::Call` nodes.
                    "call_expression" => {
                        if let Some(name) = call_target_name(inner, source) {
                            cfg.nodes[current].stmts.push(Stmt::Call { target: crate::cfg::CallTarget::Named(name), args: call_arg_identifiers(inner, source), assigned_to: None });
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
            // The value is a direct child (an `identifier`), same shape as
            // Java's — unlike Go's, which nests it inside an
            // `expression_list`.
            let value = stmt.named_child(0).filter(|n| n.kind() == "identifier").map(|n| text(n, source).to_string());
            cfg.nodes[current].stmts.push(Stmt::Return { value });
            cfg.edges.push((current, cfg.exit, EdgeKind::Fallthrough));
            cfg.push_node(stmt.start_position().row as u32 + 1)
        }
        "if_statement" => lower_if(cfg, stmt, source, current),
        "for_statement" | "while_statement" => lower_loop(cfg, stmt, source, current),
        _ => {
            cfg.nodes[current].stmts.push(Stmt::Other(text(stmt, source).to_string()));
            current
        }
    }
}

/// `alternative`'s own single named child is either another `if_statement`
/// (an `else if` chain) or a `statement_block` (a plain `else`) — verified
/// against the real grammar, not assumed from another language's shape.
fn lower_if(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    if let Some(cond) = stmt.child_by_field_name("condition") {
        cfg.nodes[current].stmts.push(Stmt::Other(format!("if {}", text(cond, source))));
    }

    let true_start = cfg.push_node(stmt.start_position().row as u32 + 1);
    cfg.edges.push((current, true_start, EdgeKind::True));
    let true_end = match stmt.child_by_field_name("consequence") {
        Some(block) if block.kind() == "statement_block" => lower_block(cfg, block, source, true_start),
        Some(single) => lower_statement(cfg, single, source, true_start),
        None => true_start,
    };

    let false_end = if let Some(alt) = stmt.child_by_field_name("alternative") {
        let false_start = cfg.push_node(stmt.start_position().row as u32 + 1);
        cfg.edges.push((current, false_start, EdgeKind::False));
        match alt.named_child(0) {
            Some(inner) if inner.kind() == "statement_block" => lower_block(cfg, inner, source, false_start),
            Some(inner) if inner.kind() == "if_statement" => lower_if(cfg, inner, source, false_start),
            _ => false_start,
        }
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

/// Shared lowering for `for`/`while` — both have a `body` field and the
/// same loop-with-back-edge shape; the header's own init/condition/update
/// content isn't modeled, same as the other lowering modules.
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

    fn lower(source: &str, language: autoreview_langsupport::Language) -> Cfg<Stmt> {
        let mut parser = autoreview_langsupport::parser_for(language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let fn_node = root.named_children(&mut cursor).find(|n| n.kind() == "function_declaration").expect("no function_declaration found");
        lower_function(source.as_bytes(), fn_node)
    }

    #[test]
    fn lowers_a_straight_line_function() {
        let cfg = lower("function f() {\n  let x = 1;\n  return x;\n}\n", autoreview_langsupport::Language::JavaScript);
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Assign { lhs, .. } if lhs == "x"))), "got: {:#?}", cfg.nodes);
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Return { value: Some(v) } if v == "x"))), "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn lowers_an_if_else_into_true_false_edges() {
        let cfg = lower("function f(x) {\n  if (x > 0) {\n    return x;\n  } else {\n    return 0;\n  }\n}\n", autoreview_langsupport::Language::JavaScript);
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::True));
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::False));
    }

    #[test]
    fn lowers_a_null_assignment() {
        let cfg = lower("function f() {\n  let e = null;\n  return e;\n}\n", autoreview_langsupport::Language::JavaScript);
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Assign { lhs, rhs: RhsShape::NilLiteral } if lhs == "e"))), "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn lowers_a_for_loop_with_a_back_edge() {
        let cfg = lower("function f() {\n  for (let i = 0; i < 10; i++) {\n    let y = i;\n  }\n}\n", autoreview_langsupport::Language::JavaScript);
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::Loop));
    }

    #[test]
    fn lowers_a_while_loop_with_a_back_edge() {
        let cfg = lower("function f() {\n  while (true) {\n    let y = 1;\n  }\n}\n", autoreview_langsupport::Language::JavaScript);
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::Loop));
    }

    #[test]
    fn recognizes_a_call_assigned_to_a_variable() {
        let cfg = lower("function f() {\n  let e = helper();\n  return e;\n}\n", autoreview_langsupport::Language::JavaScript);
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), assigned_to: Some(a), .. } if name == "helper" && a == "e"))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_a_member_call_assigned_to_a_variable() {
        let cfg = lower("function f(req) {\n  let q = req.query(sql);\n  return q;\n}\n", autoreview_langsupport::Language::JavaScript);
        assert!(
            cfg.nodes
                .iter()
                .any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, assigned_to: Some(a) } if name == "req.query" && a == "q" && args.contains(&"sql".to_string())))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_a_bare_call_statement_with_no_assignment() {
        let cfg = lower("function f(stmt, sql) {\n  stmt.execute(sql);\n}\n", autoreview_langsupport::Language::JavaScript);
        assert!(
            cfg.nodes
                .iter()
                .any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, assigned_to: None } if name == "stmt.execute" && args.contains(&"sql".to_string())))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn works_identically_against_typescript_source() {
        // Same shapes, parsed with the other grammar — proves this module
        // isn't accidentally JS-grammar-specific despite handling both.
        let cfg = lower("function f(req: Request): string {\n  const q: string = req.query(sql);\n  return q;\n}\n", autoreview_langsupport::Language::TypeScript);
        assert!(
            cfg.nodes
                .iter()
                .any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), assigned_to: Some(a), .. } if name == "req.query" && a == "q"))),
            "got: {:#?}",
            cfg.nodes
        );
    }
}
