//! Kotlin class/field/method extraction via a real tree-sitter parse,
//! mirroring `java.rs`'s shape and restraint as closely as the grammar
//! allows. Grammar node kinds below were verified empirically against
//! `tree-sitter-kotlin-ng` (the actively-maintained `tree-sitter-grammars`
//! org fork — see the crate-level docs for why this crate, not the stale
//! `tree-sitter-kotlin` originally evaluated and rejected).
//!
//! Two real grammar differences from Java that shape this module:
//!
//! 1. **No field names.** `tree-sitter-kotlin-ng` doesn't declare named
//!    fields (`child_by_field_name` returns `None` everywhere tested) —
//!    everything here is positional/kind-based child access instead,
//!    verified against hand-written fixtures via a throwaway dump tool
//!    during this session (not assumed from docs). More fragile than
//!    Java's field-based extraction to unusual formatting in principle,
//!    but Kotlin's grammar keeps a stable child order for the shapes this
//!    module cares about.
//! 2. **Calls and field access share one shape.** `a.b` (field access) and
//!    `a.b()` (method call) are both `navigation_expression`; a call is a
//!    `call_expression` wrapping that `navigation_expression` plus
//!    `value_arguments`. Java's grammar keeps `field_access`/
//!    `method_invocation` as distinct node kinds with no overlap; here,
//!    visiting a `call_expression`'s callee `navigation_expression` via
//!    the generic recursive walk would double-count it as a plain field
//!    access too — `walk_accesses`'s `navigation_expression` arm
//!    explicitly skips a node that's the callee of a parent
//!    `call_expression` (already handled by that arm) to avoid this.
//!
//! Same known, deliberate limitation as Java: own-field access is only
//! recognized via explicit `this.field`, not bare `field` — even more of
//! an under-count here than for Java, since bare property access without
//! `this.` is the dominant Kotlin idiom (Java at least commonly uses
//! `this.field`). Kept consistent with Java's exact heuristic rather than
//! inventing a bare-identifier-matches-a-known-field heuristic, which
//! would risk new false positives (matching a local/param that happens to
//! share a field's name) — the existing Stage 3.5 semantic-verify pass
//! (Feature Envy is already unioned into `semantic_ids`) is the intended
//! safety net for this under-count, same as it is for Java.

use std::path::Path;

use tree_sitter::Node;

use crate::model::{AccessRef, CallChain, ForeignAccessRef, MethodDecl, NamedSlot, TypeDecl};

fn text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn line_of(node: Node) -> u32 {
    node.start_position().row as u32 + 1
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

/// Walks the whole tree looking for `class_declaration` nodes — Kotlin's
/// grammar reuses this one kind for `class`, `interface`, `object`, and
/// `enum class`; only ones whose first child is literally the `class`
/// keyword are real classes (interfaces have no methods/fields worth
/// tracking the same way, and Refused Bequest specifically needs
/// class-to-class inheritance).
pub fn extract_types(tree: &tree_sitter::Tree, source: &[u8], file: &Path) -> Vec<TypeDecl> {
    let mut types = Vec::new();
    collect_classes(tree.root_node(), source, file, &mut types);
    types
}

fn collect_classes(node: Node, source: &[u8], file: &Path, out: &mut Vec<TypeDecl>) {
    if node.kind() == "class_declaration" && node.child(0).map(|c| c.kind()) == Some("class") {
        if let Some(type_decl) = extract_class(node, source, file) {
            out.push(type_decl);
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_classes(child, source, file, out);
    }
}

/// A field declared as a `val`/`var` primary-constructor parameter, e.g.
/// `class Sub(val name: String)` — idiomatic Kotlin, and a real field the
/// same way a `class_body` `property_declaration` is. A constructor
/// parameter with neither keyword is a plain parameter, not a field.
fn extract_constructor_properties(class_node: Node, source: &[u8]) -> Vec<NamedSlot> {
    let Some(primary_ctor) = first_child_of_kind(class_node, "primary_constructor") else { return Vec::new() };
    let Some(params) = first_child_of_kind(primary_ctor, "class_parameters") else { return Vec::new() };
    children_of_kind(params, "class_parameter")
        .into_iter()
        .filter(|p| first_child_of_kind(*p, "val").is_some() || first_child_of_kind(*p, "var").is_some())
        .filter_map(|p| {
            let name = first_child_of_kind(p, "identifier")?;
            let type_text = first_child_of_kind(p, "user_type").map(|t| text(t, source).to_string()).unwrap_or_default();
            Some(NamedSlot { name: text(name, source).to_string(), type_text })
        })
        .collect()
}

/// The raw `extends`-equivalent target: the one `delegation_specifier`
/// (there may be several — one superclass plus any number of implemented
/// interfaces) whose child is a `constructor_invocation`. Kotlin syntax
/// itself disambiguates this: extending a class always takes a
/// constructor call (`: Base()`), implementing an interface never does
/// (`: Greeter`), so this is a real, not heuristic, distinction.
fn extract_superclass(class_node: Node, source: &[u8]) -> Option<String> {
    let specifiers = first_child_of_kind(class_node, "delegation_specifiers")?;
    let specifier = children_of_kind(specifiers, "delegation_specifier").into_iter().find(|s| first_child_of_kind(*s, "constructor_invocation").is_some())?;
    let invocation = first_child_of_kind(specifier, "constructor_invocation")?;
    let user_type = first_child_of_kind(invocation, "user_type")?;
    first_child_of_kind(user_type, "identifier").map(|n| text(n, source).to_string())
}

fn extract_class(node: Node, source: &[u8], file: &Path) -> Option<TypeDecl> {
    let name_node = first_child_of_kind(node, "identifier")?;
    let name = text(name_node, source).to_string();
    let superclass = extract_superclass(node, source);
    let body = first_child_of_kind(node, "class_body")?;

    let mut fields = extract_constructor_properties(node, source);
    {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if child.kind() == "property_declaration" {
                let Some(var_decl) = first_child_of_kind(child, "variable_declaration") else { continue };
                let Some(field_name) = first_child_of_kind(var_decl, "identifier") else { continue };
                let type_text = first_child_of_kind(var_decl, "user_type").map(|t| text(t, source).to_string()).unwrap_or_default();
                fields.push(NamedSlot { name: text(field_name, source).to_string(), type_text });
            }
        }
    }

    let mut methods = Vec::new();
    {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if child.kind() == "function_declaration" {
                if let Some(m) = extract_method(child, source, file, &name, &fields) {
                    methods.push(m);
                }
            }
        }
    }

    Some(TypeDecl { name, file: file.to_path_buf(), start_line: line_of(node), fields, methods, superclass })
}

fn extract_method(node: Node, source: &[u8], file: &Path, owner_type: &str, fields: &[NamedSlot]) -> Option<MethodDecl> {
    let name_node = first_child_of_kind(node, "identifier")?;
    let name = text(name_node, source).to_string();

    let mut params = Vec::new();
    if let Some(params_node) = first_child_of_kind(node, "function_value_parameters") {
        for p in children_of_kind(params_node, "parameter") {
            let Some(param_name) = first_child_of_kind(p, "identifier") else { continue };
            let type_text = first_child_of_kind(p, "user_type").map(|t| text(t, source).to_string()).unwrap_or_default();
            params.push(NamedSlot { name: text(param_name, source).to_string(), type_text });
        }
    }

    // The return type `user_type` is a *direct* child of function_declaration
    // (after the `:`), not inside function_value_parameters — take the last
    // direct user_type child so a parameter's own type (nested one level
    // deeper, inside `parameter`) never gets mistaken for it.
    let return_type_text = children_of_kind(node, "user_type").last().map(|t| text(*t, source).to_string());

    let mut own_field_accesses = Vec::new();
    let mut foreign_accesses = Vec::new();
    let mut chains = Vec::new();
    let mut trivial_body = false;
    if let Some(function_body) = first_child_of_kind(node, "function_body") {
        if let Some(block) = first_child_of_kind(function_body, "block") {
            walk_accesses(block, source, fields, &params, &mut own_field_accesses, &mut foreign_accesses);
            walk_chains(block, source, &mut chains);
            trivial_body = is_trivial_body(block, source);
        }
    }

    Some(MethodDecl {
        name,
        owner_type: owner_type.to_string(),
        file: file.to_path_buf(),
        start_line: line_of(node),
        end_line: node.end_position().row as u32 + 1,
        params,
        return_type_text,
        own_field_accesses,
        foreign_accesses,
        chains,
        is_trivial_body: trivial_body,
    })
}

/// Mirrors Java's Refused Bequest signal: an empty body, or a single
/// statement that throws `NotImplementedError`/an
/// `UnsupportedOperationException`-shaped call (Kotlin's own stdlib
/// convention is `TODO()`, which expands to `throw NotImplementedError(...)`
/// — checked directly here since the raw source text says `TODO()`, not
/// the expansion).
fn is_trivial_body(block: Node, source: &[u8]) -> bool {
    let mut cursor = block.walk();
    let statements: Vec<Node> = block.named_children(&mut cursor).collect();
    match statements.as_slice() {
        [] => true,
        [only] => {
            let t = text(*only, source);
            t.contains("UnsupportedOperationException") || t.contains("NotImplementedError") || t.trim() == "TODO()"
        }
        _ => false,
    }
}

/// Is `node` (a `call_expression`) the callee position of a parent
/// `navigation_expression` that's itself the callee of a grandparent
/// `call_expression`? That's the shape of an inner link in a call chain
/// (`a.b().c()`'s `a.b()` relative to the outer `.c()`), already covered
/// when the outer node is visited — skipped here to avoid double-counting.
fn is_inner_chain_link(node: Node) -> bool {
    let Some(parent) = node.parent() else { return false };
    if parent.kind() != "navigation_expression" {
        return false;
    }
    if parent.child(0).map(|c| c.id()) != Some(node.id()) {
        return false;
    }
    parent.parent().map(|gp| gp.kind() == "call_expression").unwrap_or(false)
}

/// Is `node` (a `navigation_expression`) the callee of a parent
/// `call_expression`? If so, `walk_accesses`'s `call_expression` arm
/// already handles it as a foreign method-call access — the generic
/// `navigation_expression` arm must not also count it as a field access,
/// or a call like `other.getQuantity()` would be recorded twice.
fn is_call_callee(node: Node) -> bool {
    let Some(parent) = node.parent() else { return false };
    parent.kind() == "call_expression" && parent.child(0).map(|c| c.id()) == Some(node.id())
}

fn walk_accesses(node: Node, source: &[u8], fields: &[NamedSlot], params: &[NamedSlot], own: &mut Vec<AccessRef>, foreign: &mut Vec<ForeignAccessRef>) {
    match node.kind() {
        "navigation_expression" if !is_call_callee(node) => {
            if let (Some(object), Some(member)) = (node.child(0), node.child(2)) {
                if member.kind() == "identifier" {
                    let member_name = text(member, source).to_string();
                    let line = line_of(node);
                    if object.kind() == "this_expression" {
                        if fields.iter().any(|f| f.name == member_name) {
                            own.push(AccessRef { field_name: member_name, line });
                        }
                    } else if object.kind() == "identifier" {
                        let receiver_name = text(object, source).to_string();
                        if let Some(param) = params.iter().find(|p| p.name == receiver_name) {
                            foreign.push(ForeignAccessRef { receiver_name, receiver_type: Some(param.type_text.clone()), member_name, line });
                        }
                    }
                }
            }
        }
        "call_expression" => {
            if let Some(nav) = node.child(0).filter(|c| c.kind() == "navigation_expression") {
                if let (Some(object), Some(member)) = (nav.child(0), nav.child(2)) {
                    if object.kind() == "identifier" && member.kind() == "identifier" {
                        let receiver_name = text(object, source).to_string();
                        if let Some(param) = params.iter().find(|p| p.name == receiver_name) {
                            let member_name = text(member, source).to_string();
                            foreign.push(ForeignAccessRef { receiver_name, receiver_type: Some(param.type_text.clone()), member_name, line: line_of(node) });
                        }
                    }
                }
                // `this.foo()` (object.kind() == "this_expression") is an
                // own-method call, not tracked here — same restraint as Java.
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_accesses(child, source, fields, params, own, foreign);
    }
}

fn walk_chains(node: Node, source: &[u8], out: &mut Vec<CallChain>) {
    if node.kind() == "call_expression" && !is_inner_chain_link(node) {
        out.push(build_chain(node, source));
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_chains(child, source, out);
    }
}

/// Unwraps a chain of `call_expression` links starting at `node` (the
/// outermost call) down to its root expression, collecting member names in
/// root-to-tip order. A bare call with no explicit receiver (`foo()`, an
/// implicit-`this` call) has no `navigation_expression` wrapper at all —
/// treated the same as Java's implicit-`this` case: root is `"this"`, and
/// the bare call's own name is its one member.
fn build_chain(node: Node, source: &[u8]) -> CallChain {
    let line = line_of(node);
    let mut member_names_rev = Vec::new();
    let mut current = node;
    loop {
        if current.kind() != "call_expression" {
            member_names_rev.reverse();
            return CallChain { root_text: text(current, source).to_string(), depth: member_names_rev.len(), line, member_names: member_names_rev };
        }
        match current.child(0) {
            Some(callee) if callee.kind() == "navigation_expression" => {
                let member_text = callee.child(2).map(|m| text(m, source).to_string()).unwrap_or_default();
                member_names_rev.push(member_text);
                match callee.child(0) {
                    Some(object) => current = object,
                    None => {
                        member_names_rev.reverse();
                        return CallChain { root_text: String::new(), depth: member_names_rev.len(), line, member_names: member_names_rev };
                    }
                }
            }
            Some(callee) => {
                // A bare call: the callee identifier is both the implicit
                // root and this link's own member name.
                member_names_rev.push(text(callee, source).to_string());
                member_names_rev.reverse();
                return CallChain { root_text: "this".to_string(), depth: member_names_rev.len(), line, member_names: member_names_rev };
            }
            None => {
                member_names_rev.reverse();
                return CallChain { root_text: String::new(), depth: member_names_rev.len(), line, member_names: member_names_rev };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn parse(source: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_kotlin_ng::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        (tree, source.as_bytes().to_vec())
    }

    fn extract(source: &str) -> Vec<TypeDecl> {
        let (tree, bytes) = parse(source);
        extract_types(&tree, &bytes, &PathBuf::from("Sample.kt"))
    }

    #[test]
    fn extracts_a_simple_class_with_a_field_and_a_method() {
        let types = extract("class Widget {\n    var quantity: Int = 0\n\n    fun total(): Int {\n        return this.quantity\n    }\n}\n");
        assert_eq!(types.len(), 1);
        let widget = &types[0];
        assert_eq!(widget.name, "Widget");
        assert_eq!(widget.fields, vec![NamedSlot { name: "quantity".into(), type_text: "Int".into() }]);
        assert_eq!(widget.methods.len(), 1);
        assert_eq!(widget.methods[0].name, "total");
        assert_eq!(widget.methods[0].own_field_accesses, vec![AccessRef { field_name: "quantity".into(), line: 5 }]);
    }

    #[test]
    fn interfaces_are_not_extracted_as_classes() {
        let types = extract("interface Greeter {\n    fun greet()\n}\n");
        assert!(types.is_empty());
    }

    #[test]
    fn extracts_constructor_declared_val_properties_as_fields() {
        let types = extract("class Widget(val name: String, var count: Int) {\n}\n");
        assert_eq!(
            types[0].fields,
            vec![NamedSlot { name: "name".into(), type_text: "String".into() }, NamedSlot { name: "count".into(), type_text: "Int".into() }]
        );
    }

    #[test]
    fn a_constructor_parameter_without_val_or_var_is_not_a_field() {
        let types = extract("class Widget(name: String) {\n}\n");
        assert!(types[0].fields.is_empty());
    }

    #[test]
    fn extracts_the_superclass_but_not_an_interface() {
        let types = extract("interface Greeter {\n    fun greet()\n}\nclass Sub : Base(), Greeter {\n}\n");
        assert_eq!(types.len(), 1, "the interface itself shouldn't be extracted as a class");
        assert_eq!(types[0].superclass.as_deref(), Some("Base"));
    }

    #[test]
    fn a_class_with_no_superclass_has_none() {
        let types = extract("class Standalone {\n}\n");
        assert_eq!(types[0].superclass, None);
    }

    #[test]
    fn a_foreign_field_access_via_a_parameter_is_recorded() {
        let types = extract("class W {\n    fun f(other: Customer) {\n        val b = other.balance\n    }\n}\n");
        let foreign = &types[0].methods[0].foreign_accesses;
        assert_eq!(foreign.len(), 1);
        assert_eq!(foreign[0].receiver_name, "other");
        assert_eq!(foreign[0].receiver_type.as_deref(), Some("Customer"));
        assert_eq!(foreign[0].member_name, "balance");
    }

    #[test]
    fn a_foreign_method_call_via_a_parameter_is_recorded_and_not_double_counted_as_a_field_access() {
        let types = extract("class W {\n    fun f(other: Customer): Int {\n        return other.getBalance()\n    }\n}\n");
        let m = &types[0].methods[0];
        assert_eq!(m.foreign_accesses.len(), 1, "got: {:?}", m.foreign_accesses);
        assert_eq!(m.foreign_accesses[0].member_name, "getBalance");
    }

    #[test]
    fn an_empty_method_body_is_a_trivial_body() {
        let types = extract("class Sub : Base() {\n    fun save() {\n    }\n}\n");
        assert!(types[0].methods[0].is_trivial_body);
    }

    #[test]
    fn a_lone_todo_call_is_a_trivial_body() {
        let types = extract("class Sub : Base() {\n    fun save() {\n        TODO()\n    }\n}\n");
        assert!(types[0].methods[0].is_trivial_body);
    }

    #[test]
    fn a_real_method_body_is_not_trivial() {
        let types = extract("class Sub : Base() {\n    fun save() {\n        this.persist()\n    }\n}\n");
        assert!(!types[0].methods[0].is_trivial_body);
    }

    #[test]
    fn a_message_chain_is_recorded_as_a_single_maximal_chain() {
        let types = extract("class W {\n    fun f(o: Owner): String {\n        return o.getAddress().getCity().uppercase()\n    }\n}\n");
        let chains = &types[0].methods[0].chains;
        assert_eq!(chains.len(), 1, "expected exactly one maximal chain, got {chains:?}");
        assert_eq!(chains[0].root_text, "o");
        assert_eq!(chains[0].depth, 3);
        assert_eq!(chains[0].member_names, vec!["getAddress".to_string(), "getCity".to_string(), "uppercase".to_string()]);
    }

    #[test]
    fn a_lone_method_call_is_a_depth_one_chain() {
        let types = extract("class W {\n    fun f(c: Customer) {\n        c.getBalance()\n    }\n}\n");
        let chains = &types[0].methods[0].chains;
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].depth, 1);
        assert_eq!(chains[0].root_text, "c");
    }

    #[test]
    fn two_independent_calls_in_the_same_method_produce_two_separate_chains() {
        let types = extract("class W {\n    fun f(c: Customer, w: Widget) {\n        c.getBalance()\n        w.getQuantity()\n    }\n}\n");
        assert_eq!(types[0].methods[0].chains.len(), 2);
    }
}
