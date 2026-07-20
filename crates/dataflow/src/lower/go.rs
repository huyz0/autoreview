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
        // `nil` is its own grammar node kind, not an `identifier` (verified
        // via `--debug-query=ast`) — distinct from a regular identifier
        // that happens to be named `nil` (which can't occur; `nil` is a
        // predeclared identifier, but the parser still tokenizes the
        // literal itself under this dedicated kind).
        "nil" => RhsShape::NilLiteral,
        "identifier" => RhsShape::Var(text(node, source).to_string()),
        "unary_expression" => {
            let full = text(node, source);
            match node.child_by_field_name("operand") {
                Some(operand) if operand.kind() == "identifier" && full.starts_with('&') => RhsShape::AddressOf { of: text(operand, source).to_string() },
                _ => RhsShape::Unknown,
            }
        }
        "binary_expression" => classify_binary_expression(node, source),
        _ => RhsShape::Unknown,
    }
}

/// Recognizes string concatenation (`a + b + "literal"`) — the operator
/// token sits between the `left`/`right` fields as an anonymous child,
/// identified by structural exclusion (not `left`'s or `right`'s own
/// node) rather than a named field, since `binary_expression` doesn't
/// expose one. Anything other than a bare `+` (comparisons, `&&`, etc.)
/// stays `Unknown` here — this crate's other binary-expression handling
/// (`recognize_nil_guard`) covers the comparison case separately.
fn classify_binary_expression(node: Node, source: &[u8]) -> RhsShape {
    let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else { return RhsShape::Unknown };
    let mut cursor = node.walk();
    let is_plus = node.children(&mut cursor).any(|c| c.id() != left.id() && c.id() != right.id() && text(c, source) == "+");
    if !is_plus {
        return RhsShape::Unknown;
    }
    let mut parts = Vec::new();
    collect_identifiers(left, source, &mut parts);
    collect_identifiers(right, source, &mut parts);
    parts.sort();
    parts.dedup();
    RhsShape::Concat { parts }
}

/// Lowers a call expression's arguments to the flat identifier list
/// `Stmt::Call` carries. A bare identifier argument lowers to its name;
/// an address-of argument (`&x`) lowers to `"&x"` — a pragmatic encoding
/// (rather than a second `Vec` field on `Stmt::Call`) that keeps this
/// variant's shape stable for the rules that don't care about the
/// distinction, while still letting `go_loopvar_address_capture` check
/// for it via a simple prefix match. Anything else (literals, nested
/// expressions) is omitted.
fn call_arg_identifiers(call: Node, source: &[u8]) -> Vec<String> {
    call.child_by_field_name("arguments")
        .map(|args| {
            let mut cursor = args.walk();
            args.named_children(&mut cursor)
                .filter_map(|n| match n.kind() {
                    "identifier" => Some(text(n, source).to_string()),
                    "unary_expression" if text(n, source).starts_with('&') => n.child_by_field_name("operand").filter(|o| o.kind() == "identifier").map(|o| format!("&{}", text(o, source))),
                    // `full[0]` — indexing into a variable still "uses"
                    // it (relevant both to append-shared-backing-array's
                    // "is this variable still used" check and to taint
                    // propagation), so surface the indexed variable's own
                    // name rather than dropping it because the argument
                    // as a whole isn't a bare identifier.
                    "index_expression" => n.child_by_field_name("operand").filter(|o| o.kind() == "identifier").map(|o| text(o, source).to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Lowers a `left := right` / `left = right` shaped statement (both
/// `short_var_declaration` and `assignment_statement` use the same
/// `left`/`right` field names in this grammar) into a `Stmt`. Any call
/// expression on the right (not just `append`) lowers to `Stmt::Call`
/// rather than a generic `Assign` — rules that need interprocedural
/// resolution (e.g. `typed-nil-interface-return`) need to see the call
/// target, not just "this variable was reassigned to something unknown".
/// Resolves a call expression's function target to a name usable for
/// taint-rule/interprocedural matching: a bare `name(...)` call lowers to
/// `"name"`; a selector call (`pkg.Func(...)` or `recv.Method(...)`)
/// lowers to `"operand.field"` (e.g. `"exec.Command"`, `"r.FormValue"`).
/// This is syntactic, not type-resolved — `r.FormValue` and
/// `otherThing.FormValue` both lower to their own literal qualified
/// text, so taint-rule sink/source patterns that only care about the
/// trailing method name (not the specific receiver) should match on the
/// suffix after the last `.`, not the whole string.
fn call_target_name(call: Node, source: &[u8]) -> Option<String> {
    let func = call.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => Some(text(func, source).to_string()),
        "selector_expression" => {
            let operand = func.child_by_field_name("operand")?;
            let field = func.child_by_field_name("field")?;
            Some(format!("{}.{}", text(operand, source), text(field, source)))
        }
        _ => None,
    }
}

/// A `pkg.Type{Field: value, ...}` composite literal's type name (`"exec.Cmd"`),
/// unwrapping an optional leading `&`. Only `qualified_type` (`pkg.Type`) and
/// bare `type_identifier` (a same-package type) are recognized, matching
/// `call_target_name`'s own scope — anything else (generic instantiation,
/// array/slice literal types) isn't a struct sink candidate.
fn composite_literal_operand(rhs_node: Node) -> Option<Node> {
    match rhs_node.kind() {
        "composite_literal" => Some(rhs_node),
        "unary_expression" => rhs_node.child_by_field_name("operand").filter(|o| o.kind() == "composite_literal"),
        _ => None,
    }
}

/// Lowers `pkg.Type{Field: value, ...}` composite literal construction
/// (see `composite_literal_operand` for the recognized shapes) into one
/// synthetic `Stmt::Call` per keyed field whose value is a plain
/// identifier — `target: Named("pkg.Type{Field}")`, so a taint rule's
/// sink pattern can single out exactly the dangerous field (e.g.
/// `exec.Cmd{Path}`) rather than treating every field of the struct as
/// equally sensitive. Modeling the literal as the sink itself (rather
/// than waiting for a later `cmd.Run()`) matches how `exec.Command(...)`
/// is already treated as dangerous at the call site, not at whatever
/// later line actually executes it. Fields whose value isn't a bare
/// identifier (a literal, a nested expression) are skipped — same
/// precision-over-generality tradeoff as `call_arg_identifiers`.
fn lower_composite_literal_fields(composite: Node, source: &[u8]) -> Vec<Stmt> {
    let Some(type_node) = composite.child_by_field_name("type") else { return Vec::new() };
    let type_name = match type_node.kind() {
        "qualified_type" => {
            let (Some(pkg), Some(name)) = (type_node.child_by_field_name("package"), type_node.child_by_field_name("name")) else { return Vec::new() };
            format!("{}.{}", text(pkg, source), text(name, source))
        }
        "type_identifier" => text(type_node, source).to_string(),
        _ => return Vec::new(),
    };
    let Some(body) = composite.child_by_field_name("body") else { return Vec::new() };
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .filter(|n| n.kind() == "keyed_element")
        .filter_map(|kv| {
            let key = kv.child_by_field_name("key")?;
            let value = kv.child_by_field_name("value")?;
            let value_ident = value.named_child(0).filter(|n| n.kind() == "identifier")?;
            Some(Stmt::Call {
                target: crate::cfg::CallTarget::Named(format!("{type_name}{{{}}}", text(key, source))),
                args: vec![text(value_ident, source).to_string()],
                assigned_to: None,
            })
        })
        .collect()
}

fn lower_assign_like(node: Node, source: &[u8]) -> Vec<Stmt> {
    let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else {
        return vec![Stmt::Other(text(node, source).to_string())];
    };
    if right.named_child_count() != 1 {
        return vec![Stmt::Other(text(node, source).to_string())];
    }
    let rhs_node = right.named_child(0).unwrap();

    // The common `value, err := f()` idiom — most of Go's standard
    // library returns `(value, error)`, so without this case almost no
    // realistic sink call (db.Query, os.Open, ...) would ever lower to
    // `Stmt::Call` at all. Only the primary (first) value is tracked;
    // the second binding (idiomatically `err`) isn't modeled — this
    // isn't general tuple-assignment support, just this one shape.
    if left.named_child_count() == 2 && rhs_node.kind() == "call_expression" {
        let primary = left.named_child(0).unwrap();
        if primary.kind() == "identifier" && text(primary, source) != "_" {
            if let Some(name) = call_target_name(rhs_node, source) {
                let args = call_arg_identifiers(rhs_node, source);
                return vec![Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, assigned_to: Some(text(primary, source).to_string()) }];
            }
        }
        return vec![Stmt::Other(text(node, source).to_string())];
    }

    if left.named_child_count() != 1 {
        return vec![Stmt::Other(text(node, source).to_string())];
    }
    let lhs_node = left.named_child(0).unwrap();
    if lhs_node.kind() != "identifier" {
        return vec![Stmt::Other(text(node, source).to_string())];
    }
    let lhs = text(lhs_node, source).to_string();

    if rhs_node.kind() == "call_expression" {
        if let Some(name) = call_target_name(rhs_node, source) {
            let args = call_arg_identifiers(rhs_node, source);
            return vec![Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, assigned_to: Some(lhs) }];
        }
    }

    if let Some(composite) = composite_literal_operand(rhs_node) {
        let synthetic = lower_composite_literal_fields(composite, source);
        if !synthetic.is_empty() {
            return synthetic;
        }
    }

    vec![Stmt::Assign { lhs, rhs: classify_rhs(rhs_node, source) }]
}

/// Lowers `var x *T` (no initializer — starts out nil) or `var x *T =
/// expr` into a `Stmt`. Only the single-spec, single-name case is
/// modeled, same precision-over-generality tradeoff as
/// `lower_assign_like`.
fn lower_var_declaration(node: Node, source: &[u8]) -> Stmt {
    let mut cursor = node.walk();
    let specs: Vec<Node> = node.named_children(&mut cursor).filter(|n| n.kind() == "var_spec").collect();
    if specs.len() != 1 {
        return Stmt::Other(text(node, source).to_string());
    }
    let spec = specs[0];
    let (Some(name_node), Some(type_node)) = (spec.child_by_field_name("name"), spec.child_by_field_name("type")) else {
        return Stmt::Other(text(node, source).to_string());
    };
    let lhs = text(name_node, source).to_string();

    if let Some(value) = spec.child_by_field_name("value") {
        return Stmt::Assign { lhs, rhs: classify_rhs(value, source) };
    }

    // No initializer: a declared-but-unassigned pointer starts out nil;
    // anything else (a slice, a struct, an int, ...) starts out its
    // zero value, which this rule set doesn't currently need to model.
    if type_node.kind() == "pointer_type" {
        Stmt::Assign { lhs, rhs: RhsShape::NilLiteral }
    } else {
        Stmt::Other(text(node, source).to_string())
    }
}

/// Does this function's declared result include a trailing `error`
/// (`func f() error` or `func f() (..., error)`)? Grammar shapes
/// verified via `--debug-query=ast`: a single unnamed result is a bare
/// `result: type_identifier`; a multi-value result is `result:
/// parameter_list` with one `parameter_declaration` per value.
pub fn function_returns_error(fn_node: Node, source: &[u8]) -> bool {
    let Some(result) = fn_node.child_by_field_name("result") else { return false };
    match result.kind() {
        "type_identifier" => text(result, source) == "error",
        "parameter_list" => {
            let mut cursor = result.walk();
            result
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "parameter_declaration")
                .last()
                .and_then(|last| last.child_by_field_name("type"))
                .is_some_and(|ty| ty.kind() == "type_identifier" && text(ty, source) == "error")
        }
        _ => false,
    }
}

/// Does this function's declared result look like a single, unnamed
/// pointer type (`func f() *T`)? Returns the pointee type's text (`T`)
/// if so — used to key `FunctionSummary`s by the function's own name for
/// same-file/same-package interprocedural lookups.
pub fn function_returns_pointer(fn_node: Node) -> bool {
    fn_node.child_by_field_name("result").is_some_and(|r| r.kind() == "pointer_type")
}

/// The function's own name (`func Name(...) ...`).
pub fn function_name(fn_node: Node, source: &[u8]) -> Option<String> {
    fn_node.child_by_field_name("name").map(|n| text(n, source).to_string())
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
            cfg.nodes[current].stmts.extend(lower_assign_like(stmt, source));
            current
        }
        "var_declaration" => {
            cfg.nodes[current].stmts.push(lower_var_declaration(stmt, source));
            current
        }
        "return_statement" => {
            // `return_statement`'s value sits inside an `expression_list`
            // child (not a named field, and not the identifier directly)
            // — `return x` is `return_statement -> expression_list ->
            // identifier`. Only the single-value case is modeled, same
            // as everywhere else in this lowering.
            let value = stmt
                .named_child(0)
                .filter(|list| list.kind() == "expression_list" && list.named_child_count() == 1)
                .and_then(|list| list.named_child(0))
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
        "go_statement" => {
            cfg.nodes[current].stmts.push(lower_closure_capture(stmt, source, crate::cfg::ClosureKind::Goroutine));
            current
        }
        "defer_statement" => {
            cfg.nodes[current].stmts.push(lower_closure_capture(stmt, source, crate::cfg::ClosureKind::Deferred));
            current
        }
        "expression_statement" => {
            let lowered = stmt
                .named_child(0)
                .filter(|n| n.kind() == "call_expression")
                .and_then(|call| call_target_name(call, source).map(|name| Stmt::Call { target: crate::cfg::CallTarget::Named(name), args: call_arg_identifiers(call, source), assigned_to: None }))
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

/// Recognizes `var != nil` / `nil != var` (and the `==` counterpart) —
/// the only condition shape any current rule needs a `Stmt::Guard` for.
/// Anything else (compound conditions, non-nil comparisons, etc.) yields
/// `None`; the condition still gets recorded as `Stmt::Other` text
/// regardless, so no information is lost, just not specially modeled.
fn recognize_nil_guard(cond: Node, source: &[u8]) -> Option<(String, crate::cfg::GuardOp)> {
    if cond.kind() != "binary_expression" {
        return None;
    }
    let (left, right) = (cond.child_by_field_name("left")?, cond.child_by_field_name("right")?);
    let (var_node, op_text) = match (left.kind(), right.kind()) {
        ("identifier", "nil") => (left, text(cond, source)),
        ("nil", "identifier") => (right, text(cond, source)),
        _ => return None,
    };
    let op = if op_text.contains("!=") {
        crate::cfg::GuardOp::NotEqual
    } else if op_text.contains("==") {
        crate::cfg::GuardOp::Equal
    } else {
        return None;
    };
    Some((text(var_node, source).to_string(), op))
}

fn lower_if(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    // Go's `if v := f(); v != nil { ... }` initializer clause (the
    // `if v, err := db.Query(...); err == nil {` idiom) sits in its own
    // `initializer` field, separate from `condition` — without lowering
    // it, any source/assign captured only there (a taint source, a
    // nil-check target) was silently invisible to the CFG.
    let current = if let Some(initializer) = stmt.child_by_field_name("initializer") { lower_statement(cfg, initializer, source, current) } else { current };

    // The condition expression itself isn't modeled as a `Stmt` beyond
    // the nil-guard case below — recorded as `Other` text on the branch
    // point for line-number/debug fidelity regardless.
    let nil_guard = stmt.child_by_field_name("condition").and_then(|cond| {
        cfg.nodes[current].stmts.push(Stmt::Other(format!("if {}", text(cond, source))));
        recognize_nil_guard(cond, source)
    });

    let true_start = cfg.push_node(stmt.start_position().row as u32 + 1);
    cfg.edges.push((current, true_start, EdgeKind::True));
    if let Some((var, op)) = &nil_guard {
        cfg.nodes[true_start].stmts.push(Stmt::Guard { var: var.clone(), op: *op, against: crate::cfg::GuardAgainst::Nil });
    }
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

/// Extracts a range-clause `for`'s own loop variables (`for k, v := range
/// x`) — `range_clause` is a positional child of `for_statement`, not a
/// named field (same positional-child pattern as `for_clause`, verified
/// via `--debug-query=ast`). `_` is excluded (nothing to capture). A
/// classic three-clause `for i := 0; i < n; i++` loop's `i` isn't
/// modeled here — the pre-1.22 loop-variable-scoping bug this feeds only
/// applies to range loops (matches the old heuristic's own scope).
fn range_clause_vars(stmt: Node, source: &[u8]) -> Vec<String> {
    let mut cursor = stmt.walk();
    let Some(range_clause) = stmt.named_children(&mut cursor).find(|n| n.kind() == "range_clause") else { return Vec::new() };
    let Some(left) = range_clause.child_by_field_name("left") else { return Vec::new() };
    let mut cursor = left.walk();
    left.named_children(&mut cursor).filter(|n| n.kind() == "identifier").map(|n| text(n, source).to_string()).filter(|t| t != "_").collect()
}

fn lower_for(cfg: &mut Cfg<Stmt>, stmt: Node, source: &[u8], current: NodeId) -> NodeId {
    let header = current;
    let body_start = cfg.push_node(stmt.start_position().row as u32 + 1);
    cfg.edges.push((header, body_start, EdgeKind::True));

    let loop_vars = range_clause_vars(stmt, source);
    if !loop_vars.is_empty() {
        cfg.nodes[body_start].stmts.push(Stmt::LoopVarBind { vars: loop_vars });
    }

    let body_end = if let Some(body) = stmt.child_by_field_name("body") { lower_block(cfg, body, source, body_start) } else { body_start };
    cfg.edges.push((body_end, header, EdgeKind::Loop));

    let after = cfg.push_node(stmt.end_position().row as u32 + 1);
    cfg.edges.push((header, after, EdgeKind::False));
    after
}

/// Collects every `identifier` node's text anywhere in `node`'s subtree —
/// a blunt, over-approximate free-variable set (doesn't distinguish a
/// declaration site from a use site, doesn't track the closure's own
/// local shadowing beyond the single first-statement check in
/// `lower_closure_capture`). That's an acceptable precision tradeoff
/// here: the rules consuming `ClosureCapture::captured` only care about
/// its *intersection* with the outer function's active loop variables,
/// so a superset is safe — it can't manufacture a false positive on a
/// name that isn't actually an active loop variable.
fn collect_identifiers(node: Node, source: &[u8], out: &mut Vec<String>) {
    if node.kind() == "identifier" {
        out.push(text(node, source).to_string());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_identifiers(child, source, out);
    }
}

/// Lowers `go func() { ... }()` / `defer func() { ... }()` — a bare
/// (no-parameter) function literal immediately invoked as a goroutine or
/// deferred call, the shape that captures enclosing variables by
/// reference. Anything else (`go f(x)`, `defer f(x)`, a func literal
/// *with* parameters) is safe by Go's own by-value argument-passing
/// semantics and lowers to `Stmt::Other` instead.
fn lower_closure_capture(stmt: Node, source: &[u8], kind: crate::cfg::ClosureKind) -> Stmt {
    let fallback = || Stmt::Other(text(stmt, source).to_string());
    let Some(call) = stmt.named_child(0).filter(|n| n.kind() == "call_expression") else { return fallback() };
    let Some(func) = call.child_by_field_name("function").filter(|n| n.kind() == "func_literal") else { return fallback() };
    let is_bare = func.child_by_field_name("parameters").map(|p| p.named_child_count() == 0).unwrap_or(true);
    if !is_bare {
        return fallback();
    }
    let Some(body) = func.child_by_field_name("body") else { return fallback() };

    let mut captured = Vec::new();
    collect_identifiers(body, source, &mut captured);

    // The standard pre-1.22 fix is a self-shadow copy (`v := v`) as the
    // closure's very first statement — exclude any variable shadowed
    // that way from `captured` so the rule doesn't flag an
    // already-fixed closure.
    let mut cursor = body.walk();
    if let Some(list) = body.named_children(&mut cursor).find(|n| n.kind() == "statement_list") {
        if let Some(first) = list.named_child(0) {
            if first.kind() == "short_var_declaration" {
                if let [Stmt::Assign { lhs, rhs: RhsShape::Var(rhs) }] = lower_assign_like(first, source).as_slice() {
                    if lhs == rhs {
                        captured.retain(|v| v != lhs);
                    }
                }
            }
        }
    }

    captured.sort();
    captured.dedup();
    Stmt::ClosureCapture { captured, kind }
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

    #[test]
    fn return_statement_carries_its_value_identifier() {
        // Regression test: `return_statement`'s value sits inside an
        // `expression_list` child, not directly as the identifier — an
        // earlier version of this lowering always produced `value: None`.
        let cfg = lower("package p\nfunc f() int {\n\tx := 1\n\treturn x\n}\n");
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Return { value: Some(v) } if v == "x"))), "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn lowers_an_if_statements_initializer_clause() {
        // Regression test: `if v := f(); v != nil { ... }` has its own
        // `initializer` field, separate from `condition` — it used to be
        // dropped entirely, so `v`'s assignment (and any taint source it
        // carries) was invisible to the CFG.
        let cfg = lower("package p\nfunc f() {\n\tif v := g(); v != nil {\n\t\t_ = v\n\t}\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { assigned_to: Some(a), .. } if a == "v") || matches!(s, Stmt::Assign { lhs, .. } if lhs == "v"))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn lowers_an_uninitialized_pointer_var_declaration_as_nil() {
        let cfg = lower("package p\nfunc f() *T {\n\tvar e *T\n\treturn e\n}\n");
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Assign { lhs, rhs: RhsShape::NilLiteral } if lhs == "e"))), "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn recognizes_a_call_assigned_to_a_variable_as_a_call_target() {
        let cfg = lower("package p\nfunc f() *T {\n\te := helper()\n\treturn e\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), assigned_to: Some(a), .. } if name == "helper" && a == "e"))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_a_not_equal_nil_guard() {
        let cfg = lower("package p\nfunc f() error {\n\tvar e *T\n\tif e != nil {\n\t\treturn e\n\t}\n\treturn nil\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Guard { var, op: crate::cfg::GuardOp::NotEqual, .. } if var == "e"))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn function_metadata_helpers_recognize_error_and_pointer_returns() {
        let mut parser = autoreview_langsupport::parser_for(autoreview_langsupport::Language::Go).unwrap();
        let src = "package p\nfunc a() error { return nil }\nfunc b() (int, error) { return 0, nil }\nfunc c() *T { return nil }\nfunc d() int { return 0 }\n";
        let tree = parser.parse(src, None).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let fns: Vec<_> = root.named_children(&mut cursor).filter(|n| n.kind() == "function_declaration").collect();
        assert!(function_returns_error(fns[0], src.as_bytes()));
        assert!(function_returns_error(fns[1], src.as_bytes()));
        assert!(!function_returns_error(fns[2], src.as_bytes()));
        assert!(function_returns_pointer(fns[2]));
        assert!(!function_returns_pointer(fns[0]));
        assert_eq!(function_name(fns[2], src.as_bytes()).as_deref(), Some("c"));
    }

    #[test]
    fn range_loop_binds_its_own_variables() {
        let cfg = lower("package p\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tprintln(item)\n\t}\n}\n");
        assert!(cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::LoopVarBind { vars } if vars == &vec!["item".to_string()]))), "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn a_three_clause_for_loop_does_not_bind_loop_vars() {
        let cfg = lower("package p\nfunc f() {\n\tfor i := 0; i < 3; i++ {\n\t\tprintln(i)\n\t}\n}\n");
        assert!(!cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::LoopVarBind { .. }))), "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn a_bare_goroutine_closure_captures_its_free_identifiers() {
        let cfg = lower("package p\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tgo func() {\n\t\t\tprintln(item)\n\t\t}()\n\t}\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::ClosureCapture { captured, kind: crate::cfg::ClosureKind::Goroutine } if captured.contains(&"item".to_string())))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn a_shadowed_closure_excludes_the_shadowed_var_from_captured() {
        let cfg = lower("package p\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tgo func() {\n\t\t\titem := item\n\t\t\tprintln(item)\n\t\t}()\n\t}\n}\n");
        assert!(
            !cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::ClosureCapture { captured, .. } if captured.contains(&"item".to_string())))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn a_closure_with_parameters_is_not_treated_as_a_capture() {
        let cfg = lower("package p\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tgo func(x string) {\n\t\t\tprintln(x)\n\t\t}(item)\n\t}\n}\n");
        assert!(!cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::ClosureCapture { .. }))), "got: {:#?}", cfg.nodes);
    }

    #[test]
    fn recognizes_an_address_of_argument_in_an_append_call() {
        let cfg = lower("package p\nfunc f(items []string) []*string {\n\tvar out []*string\n\tfor _, item := range items {\n\t\tout = append(out, &item)\n\t}\n\treturn out\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { args, .. } if args.contains(&"&item".to_string())))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_a_direct_address_of_assignment() {
        let cfg = lower("package p\nfunc f(items []string) {\n\tfor _, item := range items {\n\t\tp := &item\n\t\t_ = p\n\t}\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Assign { lhs, rhs: RhsShape::AddressOf { of } } if lhs == "p" && of == "item"))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_a_package_qualified_selector_call_assigned_to_a_variable() {
        let cfg = lower("package p\nfunc f(userInput string) {\n\tcmd := exec.Command(\"sh\", \"-c\", userInput)\n\t_ = cmd\n}\n");
        assert!(
            cfg.nodes.iter().any(
                |n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, assigned_to: Some(a) } if name == "exec.Command" && a == "cmd" && args.contains(&"userInput".to_string())))
            ),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_an_address_of_struct_literal_field_as_a_synthetic_call() {
        let cfg = lower("package p\nfunc f(userInput string) {\n\tcmd := &exec.Cmd{Path: userInput, Dir: \"/tmp\"}\n\t_ = cmd\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(
                |s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, assigned_to: None } if name == "exec.Cmd{Path}" && args == &vec!["userInput".to_string()])
            )),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn a_struct_literal_field_set_to_a_string_literal_is_not_lowered_to_a_synthetic_call() {
        let cfg = lower("package p\nfunc f() {\n\tcmd := &exec.Cmd{Dir: \"/tmp\"}\n\t_ = cmd\n}\n");
        assert!(
            !cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), .. } if name.starts_with("exec.Cmd")))),
            "a literal field value shouldn't lower to a synthetic call: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_a_bare_method_call_statement_with_no_assignment() {
        let cfg = lower("package p\nfunc f() {\n\tcmd.Run()\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), assigned_to: None, .. } if name == "cmd.Run"))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn indexing_into_a_variable_as_a_call_argument_still_counts_as_a_use() {
        let cfg = lower("package p\nfunc f(full []int) {\n\tprintln(full[0])\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { args, .. } if args.contains(&"full".to_string())))),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn lowers_a_value_err_tuple_assignment_from_a_call() {
        let cfg = lower("package p\nfunc f(path string) {\n\tfile, err := os.Open(path)\n\t_ = file\n\t_ = err\n}\n");
        assert!(
            cfg.nodes.iter().any(
                |n| n.stmts.iter().any(|s| matches!(s, Stmt::Call { target: crate::cfg::CallTarget::Named(name), args, assigned_to: Some(a) } if name == "os.Open" && a == "file" && args.contains(&"path".to_string())))
            ),
            "got: {:#?}",
            cfg.nodes
        );
    }

    #[test]
    fn recognizes_string_concatenation_carrying_both_operand_identifiers() {
        let cfg = lower("package p\nfunc f(userInput string) {\n\tquery := \"SELECT * FROM x WHERE id=\" + userInput\n\t_ = query\n}\n");
        assert!(
            cfg.nodes.iter().any(|n| n.stmts.iter().any(|s| matches!(s, Stmt::Assign { lhs, rhs: RhsShape::Concat { parts } } if lhs == "query" && parts.contains(&"userInput".to_string())))),
            "got: {:#?}",
            cfg.nodes
        );
    }
}
