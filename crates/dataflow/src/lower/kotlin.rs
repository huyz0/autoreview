//! Kotlin CST → `Cfg` lowering. Uses `tree-sitter-kotlin-ng` via
//! `autoreview-langsupport`, the same crate `autoreview-symindex`'s own
//! Kotlin extractor uses — and reuses *that* extractor's proven strategy
//! of walking children by `kind()` rather than `child_by_field_name`.
//!
//! Every shape below was verified against a small standalone probe
//! compiled against the *real* `tree-sitter-kotlin-ng` crate (not
//! `ast-grep`'s own `--debug-query=ast`, which uses a different bundled
//! Kotlin grammar build with different node-kind names — a documented
//! gotcha from earlier sessions, and it bit this module too during
//! development: the bundled grammar's dump showed `simple_identifier`,
//! `control_structure_body`, `jump_expression`, `call_suffix`, and a
//! `statements` wrapper node inside `function_body`; the real crate this
//! project depends on uses `identifier`, no `control_structure_body`
//! wrapper (an `if_expression`'s branches are `block` nodes directly),
//! `return_expression`, no `call_suffix` (a `call_expression` has
//! `value_arguments` directly), and `function_body -> block` with no
//! extra `statements` layer in between). A further surprise verified the
//! same way: Kotlin's `null` literal is a plain `identifier` node with
//! text `"null"`, not a dedicated node kind the way Go's `nil` and
//! Java's `null_literal` are — `classify_rhs` checks the text explicitly.
//!
//! No Kotlin-specific dataflow rule exists yet — like `java.rs`, this is
//! architectural completion of the generic CFG core across all three
//! languages, proven via this module's own lowering tests.

use tree_sitter::Node;

use crate::cfg::{Cfg, EdgeKind, NodeId, RhsShape, Stmt};

fn text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("").trim()
}

fn first_child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

fn children_of_kind<'a>(node: Node<'a>, kind: &str) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).filter(|c| c.kind() == kind).collect()
}

fn classify_rhs(node: Node, source: &[u8]) -> RhsShape {
    match node.kind() {
        "identifier" if text(node, source) == "null" => RhsShape::NilLiteral,
        "identifier" => RhsShape::Var(text(node, source).to_string()),
        _ => RhsShape::Unknown,
    }
}

/// Lowers `val x = expr` / `var x = expr`
/// (`property_declaration -> variable_declaration -> identifier`, with
/// the initializer as `property_declaration`'s own last child that isn't
/// the `variable_declaration`). Only the single-declarator,
/// has-an-initializer case is modeled.
fn lower_property_declaration(node: Node, source: &[u8]) -> Stmt {
    let fallback = || Stmt::Other(text(node, source).to_string());
    let Some(var_decl) = first_child_of_kind(node, "variable_declaration") else { return fallback() };
    let Some(name_node) = first_child_of_kind(var_decl, "identifier") else { return fallback() };
    let lhs = text(name_node, source).to_string();

    let mut cursor = node.walk();
    let value = node.children(&mut cursor).filter(|c| c.kind() != "variable_declaration" && c.is_named()).last();
    let Some(value) = value else { return fallback() };

    if value.kind() == "call_expression" {
        if let Some(func) = first_child_of_kind(value, "identifier") {
            let args = first_child_of_kind(value, "value_arguments").map(|a| children_of_kind(a, "value_argument").iter().filter_map(|va| first_child_of_kind(*va, "identifier")).map(|n| text(n, source).to_string()).collect()).unwrap_or_default();
            return Stmt::Call { target: crate::cfg::CallTarget::Named(text(func, source).to_string()), args, assigned_to: Some(lhs) };
        }
    }

    Stmt::Assign { lhs, rhs: classify_rhs(value, source) }
}

/// Lowers one function body into a `Cfg`. `fn_node` must be a
/// `function_declaration` with a `function_body` child.
pub fn lower_function(source: &[u8], fn_node: Node) -> Cfg<Stmt> {
    let mut cfg: Cfg<Stmt> = Cfg::new_empty();
    let entry = cfg.push_node(fn_node.start_position().row as u32 + 1);
    let exit = cfg.push_node(fn_node.end_position().row as u32 + 1);
    cfg.entry = entry;
    cfg.exit = exit;

    // A single-expression function body (`fun f() = expr`) has no
    // `block` child at all — not modeled, falls through untouched.
    let end = first_child_of_kind(fn_node, "function_body").and_then(|body| first_child_of_kind(body, "block")).map(|block| lower_block(&mut cfg, block, source, entry)).unwrap_or(entry);
    cfg.edges.push((end, exit, EdgeKind::Fallthrough));
    cfg
}

fn lower_block(cfg: &mut Cfg<Stmt>, block: Node, source: &[u8], mut current: NodeId) -> NodeId {
    let mut cursor = block.walk();
    for stmt in block.named_children(&mut cursor) {
        current = lower_statement(cfg, stmt, source, current);
    }
    current
}

fn lower_statement(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    match stmt.kind() {
        "property_declaration" => {
            cfg.nodes[current].stmts.push(lower_property_declaration(stmt, source));
            current
        }
        "return_expression" => {
            let value = stmt.named_child(0).filter(|n| n.kind() == "identifier").map(|n| text(n, source).to_string());
            cfg.nodes[current].stmts.push(Stmt::Return { value });
            cfg.edges.push((current, cfg.exit, EdgeKind::Fallthrough));
            cfg.push_node(stmt.start_position().row as u32 + 1)
        }
        "if_expression" => lower_if(cfg, stmt, source, current),
        "for_statement" | "while_statement" => lower_loop(cfg, stmt, source, current),
        _ => {
            cfg.nodes[current].stmts.push(Stmt::Other(text(stmt, source).to_string()));
            current
        }
    }
}

/// `if_expression`'s consequence/alternative are `block` nodes directly
/// (no wrapper the way some grammars use) — verified against the real
/// crate, see this module's doc comment.
fn lower_if(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    cfg.nodes[current].stmts.push(Stmt::Other(format!("if {}", text(stmt, source))));

    let branches = children_of_kind(stmt, "block");

    let true_start = cfg.push_node(stmt.start_position().row as u32 + 1);
    cfg.edges.push((current, true_start, EdgeKind::True));
    let true_end = if let Some(consequence) = branches.first() { lower_block(cfg, *consequence, source, true_start) } else { true_start };

    let false_end = if let Some(alternative) = branches.get(1) {
        let false_start = cfg.push_node(stmt.start_position().row as u32 + 1);
        cfg.edges.push((current, false_start, EdgeKind::False));
        lower_block(cfg, *alternative, source, false_start)
    } else {
        current
    };

    let join = cfg.push_node(stmt.end_position().row as u32 + 1);
    cfg.edges.push((true_end, join, EdgeKind::Fallthrough));
    if branches.len() > 1 {
        cfg.edges.push((false_end, join, EdgeKind::Fallthrough));
    } else {
        cfg.edges.push((current, join, EdgeKind::False));
    }
    join
}

fn lower_loop(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    let header = current;
    let body_start = cfg.push_node(stmt.start_position().row as u32 + 1);
    cfg.edges.push((header, body_start, EdgeKind::True));
    let body_end = if let Some(block) = first_child_of_kind(stmt, "block") { lower_block(cfg, block, source, body_start) } else { body_start };
    cfg.edges.push((body_end, header, EdgeKind::Loop));

    let after = cfg.push_node(stmt.end_position().row as u32 + 1);
    cfg.edges.push((header, after, EdgeKind::False));
    after
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower(source: &str) -> Cfg<Stmt> {
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Kotlin).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();
        fn find_fn(node: Node) -> Option<Node> {
            if node.kind() == "function_declaration" {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = find_fn(child) {
                    return Some(found);
                }
            }
            None
        }
        let fn_node = find_fn(root).expect("no function_declaration found");
        lower_function(source.as_bytes(), fn_node)
    }

    #[test]
    fn lowers_a_straight_line_function() {
        let cfg = lower("class Foo {\n    fun f(): Int {\n        val x = 1\n        return x\n    }\n}\n");
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Assign { lhs, .. } if lhs == "x"))), "got: {:#?}", cfg.nodes);
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Return { value: Some(v) } if v == "x"))), "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn lowers_an_if_else_into_true_false_edges() {
        let cfg = lower("class Foo {\n    fun f(x: Int): Int {\n        if (x > 0) {\n            return x\n        } else {\n            return 0\n        }\n    }\n}\n");
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::True));
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::False));
    }

    #[test]
    fn lowers_a_null_assignment() {
        let cfg = lower("class Foo {\n    fun f(): String? {\n        val e: String? = null\n        return e\n    }\n}\n");
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Assign { lhs, rhs: RhsShape::NilLiteral } if lhs == "e"))), "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn lowers_a_for_loop_with_a_back_edge() {
        let cfg = lower("class Foo {\n    fun f() {\n        for (i in 0..10) {\n            val y = i\n        }\n    }\n}\n");
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::Loop));
    }

    #[test]
    fn lowers_a_while_loop_with_a_back_edge() {
        let cfg = lower("class Foo {\n    fun f() {\n        while (true) {\n            val y = 1\n        }\n    }\n}\n");
        assert!(cfg.edges.iter().any(|(_, _, kind)| *kind == EdgeKind::Loop));
    }

    #[test]
    fn recognizes_a_call_assigned_to_a_variable() {
        let cfg = lower("class Foo {\n    fun f(): Int {\n        val e = helper()\n        return e\n    }\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), assigned_to: Some(a), .. } if name == "helper" && a == "e"))),
            "got: {:#?}",
            cfg.nodes
        );
    }
}
