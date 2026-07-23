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

/// The function's own bare name — same convention and same accepted
/// same-name-conflates-across-classes imprecision as Java's `function_name`
/// (see that doc comment) and Go's own `function_name`.
pub fn function_name(fn_node: Node, source: &[u8]) -> Option<String> {
    fn_node.child_by_field_name("name").map(|n| text(n, source).to_string())
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

/// Resolves a `call_expression`'s callee to a name usable for taint-rule
/// matching: a bare `f(...)` call lowers to `"f"`; a member call
/// (`recv.method(...)`, grammar shape `call_expression -> navigation_expression
/// -> [identifier, ".", identifier]` — verified against the real
/// `tree-sitter-kotlin-ng` crate, see this module's doc comment) lowers to
/// `"recv.method"`, matching Go's own qualified-selector convention so a
/// taint rule's sink/source pattern can match on either the bare trailing
/// name or the full qualified form.
fn call_target_name(call: Node, source: &[u8]) -> Option<String> {
    let callee = call.named_child(0)?;
    match callee.kind() {
        "identifier" => Some(text(callee, source).to_string()),
        "navigation_expression" => {
            let receiver = callee.named_child(0)?;
            let method = callee.named_child(1).filter(|n| n.kind() == "identifier")?;
            Some(format!("{}.{}", text(receiver, source), text(method, source)))
        }
        _ => None,
    }
}

fn call_arg_identifiers(call: Node, source: &[u8]) -> Vec<String> {
    first_child_of_kind(call, "value_arguments").map(|a| children_of_kind(a, "value_argument").iter().filter_map(|va| first_child_of_kind(*va, "identifier")).map(|n| text(n, source).to_string()).collect()).unwrap_or_default()
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
        if let Some(name) = call_target_name(value, source) {
            return Stmt::Call { target: crate::cfg::CallTarget::Named(name), args: call_arg_identifiers(value, source), assigned_to: Some(lhs) };
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
        // A bare call statement (`stmt.executeUpdate(sql)`, no `val`/`var`)
        // — unlike Java, Kotlin has no `expression_statement` wrapper, so a
        // `call_expression` can appear directly as a block statement.
        // Without this, a sink whose return value is discarded would be
        // invisible to the taint engine, which only inspects `Stmt::Call`.
        "call_expression" => {
            let lowered = call_target_name(stmt, source)
                .map(|name| Stmt::Call { target: crate::cfg::CallTarget::Named(name), args: call_arg_identifiers(stmt, source), assigned_to: None })
                .unwrap_or_else(|| Stmt::Other(text(stmt, source).to_string()));
            cfg.nodes[current].stmts.push(lowered);
            current
        }
        _ => {
            cfg.nodes[current].stmts.push(Stmt::Other(text(stmt, source).to_string()));
            current
        }
    }
}

/// Recognizes `x != null` / `null != x` (and the `==` counterpart) — same
/// scope as Go's/Java's own `recognize_*_guard`. Kotlin's condition field
/// is directly a `binary_expression` with `left`/`right` fields (no
/// wrapping `parenthesized_expression` the way Java's is), verified
/// against the real `tree-sitter-kotlin-ng` crate rather than ast-grep's
/// bundled dump — see this module's doc comment on why that distinction
/// matters here. Kotlin's `null` is a plain `identifier` with text
/// `"null"` (also per this module's doc comment), not a dedicated node
/// kind, so the operand is matched by kind *and* text rather than kind
/// alone.
fn recognize_null_guard(cond: Node, source: &[u8]) -> Option<(String, crate::cfg::GuardOp)> {
    if cond.kind() != "binary_expression" {
        return None;
    }
    let (left, right) = (cond.child_by_field_name("left")?, cond.child_by_field_name("right")?);
    let is_null = |n: Node| n.kind() == "identifier" && text(n, source) == "null";
    let var_node = if is_null(right) && !is_null(left) {
        left
    } else if is_null(left) && !is_null(right) {
        right
    } else {
        return None;
    };
    let op_text = text(cond, source);
    let op = if op_text.contains("!=") {
        crate::cfg::GuardOp::NotEqual
    } else if op_text.contains("==") {
        crate::cfg::GuardOp::Equal
    } else {
        return None;
    };
    Some((text(var_node, source).to_string(), op))
}

/// `if_expression`'s consequence/alternative are `block` nodes directly
/// (no wrapper the way some grammars use) — verified against the real
/// crate, see this module's doc comment.
fn lower_if(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    cfg.nodes[current].stmts.push(Stmt::Other(format!("if {}", text(stmt, source))));
    let null_guard = stmt.child_by_field_name("condition").and_then(|cond| recognize_null_guard(cond, source));

    let branches = children_of_kind(stmt, "block");

    let true_start = cfg.push_node(stmt.start_position().row as u32 + 1);
    cfg.edges.push((current, true_start, EdgeKind::True));
    if let Some((var, op)) = &null_guard {
        cfg.nodes[true_start].stmts.push(Stmt::Guard { var: var.clone(), op: *op, against: crate::cfg::GuardAgainst::Nil });
    }
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
    fn function_name_reads_the_functions_own_name() {
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Kotlin).unwrap();
        let source = "class Foo {\n    fun doWork(): Int {\n        return 1\n    }\n}\n";
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
        let fn_node = find_fn(root).unwrap();
        assert_eq!(function_name(fn_node, source.as_bytes()), Some("doWork".to_string()));
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

    #[test]
    fn recognizes_a_member_call_assigned_to_a_variable() {
        let cfg = lower("class Foo {\n    fun f(stmt: Any, sql: String): Any {\n        val e = stmt.executeQuery(sql)\n        return e\n    }\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(
                |s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, assigned_to: Some(a) } if name == "stmt.executeQuery" && a == "e" && args.contains(&"sql".to_string()))
            )),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_a_bare_member_call_statement_with_no_assignment() {
        let cfg = lower("class Foo {\n    fun f(stmt: Any, sql: String) {\n        stmt.executeUpdate(sql)\n    }\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(
                |s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, assigned_to: None } if name == "stmt.executeUpdate" && args.contains(&"sql".to_string()))
            )),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_a_not_equal_null_guard_on_the_true_branch() {
        let cfg = lower("class Foo {\n    fun f(e: String?) {\n        if (e != null) {\n            val x = 1\n        }\n    }\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Guard { var, op: crate::cfg::GuardOp::NotEqual, against: crate::cfg::GuardAgainst::Nil } if var == "e"))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_an_equal_null_guard_with_operands_reversed() {
        let cfg = lower("class Foo {\n    fun f(e: String?) {\n        if (null == e) {\n            val x = 1\n        }\n    }\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Guard { var, op: crate::cfg::GuardOp::Equal, against: crate::cfg::GuardAgainst::Nil } if var == "e"))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn a_non_null_condition_produces_no_guard() {
        let cfg = lower("class Foo {\n    fun f(x: Int) {\n        if (x > 0) {\n            val y = 1\n        }\n    }\n}\n");
        assert!(!cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Guard { .. }))), "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn comparing_two_variables_produces_no_guard_even_though_one_side_looks_like_null_text() {
        // Neither operand is the literal `null` identifier here — this
        // guards against a matcher that only checks operand *kind*
        // (identifier) without also checking the text, which would
        // wrongly treat any two-identifier comparison as a null guard.
        let cfg = lower("class Foo {\n    fun f(a: String, b: String) {\n        if (a != b) {\n            val y = 1\n        }\n    }\n}\n");
        assert!(!cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Guard { .. }))), "got: {:#?}", cfg.nodes);
    }
}
