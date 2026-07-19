//! TypeScript/TSX/JavaScript class/field/method extraction via a real
//! tree-sitter parse. Grammar node kinds below were verified empirically
//! against the real `tree-sitter-typescript`/`tree-sitter-javascript`
//! grammars (via `ast-grep run --lang ts -p "<source>" file.ts
//! --debug-query=ast` against hand-written fixtures), not assumed from
//! documentation — same discipline `java.rs`'s own module doc describes.
//!
//! One extraction pass serves all four extensions (`.ts`/`.tsx`/`.js`/
//! `.jsx`) — `class_declaration`/`method_definition`/`member_expression`/
//! `call_expression` are the same node kinds across all four grammars
//! (TSX/JSX just add JSX element parsing on top, which this extractor never
//! touches). JS/JSX files simply never populate `type_text`/
//! `return_type_text` (no type annotations exist to read), which the model
//! already treats as an empty string, not a special case.
//!
//! Known, deliberate limitations, mirroring `java.rs`'s own stated ones
//! where the shapes are analogous:
//! - Own-field access is only recognized via explicit `this.field` — same
//!   conservative-undercounting rationale as Java (a bare `field` reference
//!   is structurally indistinguishable from a local without real scope
//!   resolution).
//! - Only `method_definition` nodes count as methods — a class field
//!   initialized to an arrow function (`onClick = () => {...}`, a common
//!   React/class-property idiom) is extracted as a field, not a method;
//!   Feature Envy/message-chain analysis on those bodies is out of scope
//!   for this first pass.

use std::path::Path;

use tree_sitter::Node;

use crate::model::{AccessRef, CallChain, ForeignAccessRef, MethodDecl, NamedSlot, TypeDecl};

fn text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn line_of(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

/// Walks the whole tree looking for `class_declaration` nodes at any
/// nesting depth — same "every class becomes its own `TypeDecl`" shape as
/// `java.rs`.
pub fn extract_types(tree: &tree_sitter::Tree, source: &[u8], file: &Path) -> Vec<TypeDecl> {
    let mut types = Vec::new();
    collect_classes(tree.root_node(), source, file, &mut types);
    types
}

fn collect_classes(node: Node, source: &[u8], file: &Path, out: &mut Vec<TypeDecl>) {
    if node.kind() == "class_declaration" {
        if let Some(type_decl) = extract_class(node, source, file) {
            out.push(type_decl);
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_classes(child, source, file, out);
    }
}

/// `class Sub extends Base` — `class_heritage`/`extends_clause` are
/// positional children, not named fields (confirmed via `--debug-query=ast`:
/// no `field:` prefix in the dump), so found by kind rather than
/// `child_by_field_name`, same pattern this crate already uses elsewhere
/// (e.g. `go.rs`'s `range_clause`).
fn extract_superclass(node: Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let heritage = node.children(&mut cursor).find(|c| c.kind() == "class_heritage")?;
    let mut hcursor = heritage.walk();
    let extends_clause = heritage.children(&mut hcursor).find(|c| c.kind() == "extends_clause")?;
    extends_clause.child_by_field_name("value").map(|v| text(v, source).to_string())
}

/// Strips a `type_annotation` wrapper (`: number`) down to the actual type
/// node's text (`number`) — `type_annotation`'s only named child is the
/// real type, the colon itself is an unnamed punctuation token.
fn annotation_type_text(annotation: Node, source: &[u8]) -> String {
    annotation.named_child(0).map(|t| text(t, source).to_string()).unwrap_or_default()
}

fn is_trivial_body(body: Node, source: &[u8]) -> bool {
    let mut cursor = body.walk();
    let statements: Vec<Node> = body.named_children(&mut cursor).collect();
    match statements.as_slice() {
        [] => true,
        [only] if only.kind() == "throw_statement" => {
            let t = text(*only, source).to_ascii_lowercase();
            t.contains("not implemented") || t.contains("notimplementederror") || t.contains("unsupportedoperationexception")
        }
        _ => false,
    }
}

fn extract_class(node: Node, source: &[u8], file: &Path) -> Option<TypeDecl> {
    let name_node = node.child_by_field_name("name")?;
    let name = text(name_node, source).to_string();
    let superclass = extract_superclass(node, source);
    let body = node.child_by_field_name("body")?;

    // Fields first, in a separate pass, so a method declared before a field
    // in source order still sees the complete field list when classifying
    // its own-field accesses below — same reasoning as `java.rs`.
    let mut fields = Vec::new();
    {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if matches!(child.kind(), "public_field_definition" | "property_signature") {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let type_text = child.child_by_field_name("type").map(|t| annotation_type_text(t, source)).unwrap_or_default();
                    fields.push(NamedSlot { name: text(name_node, source).to_string(), type_text });
                }
            }
        }
    }

    let mut methods = Vec::new();
    {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if child.kind() == "method_definition" {
                if let Some(m) = extract_method(child, source, file, &name, &fields) {
                    methods.push(m);
                }
            }
        }
    }

    Some(TypeDecl { name, file: file.to_path_buf(), start_line: line_of(node), fields, methods, superclass })
}

fn extract_params(params_node: Node, source: &[u8]) -> Vec<NamedSlot> {
    let mut params = Vec::new();
    let mut cursor = params_node.walk();
    for p in params_node.named_children(&mut cursor) {
        // `required_parameter`/`optional_parameter` both use `pattern` for
        // the param name and `type` for its annotation — a plain
        // (untyped, e.g. in a .js file) parameter is just an `identifier`
        // node directly, no wrapper.
        let (name_node, type_text) = if matches!(p.kind(), "required_parameter" | "optional_parameter") {
            (p.child_by_field_name("pattern"), p.child_by_field_name("type").map(|t| annotation_type_text(t, source)).unwrap_or_default())
        } else {
            (Some(p), String::new())
        };
        if let Some(name_node) = name_node.filter(|n| n.kind() == "identifier") {
            params.push(NamedSlot { name: text(name_node, source).to_string(), type_text });
        }
    }
    params
}

fn extract_method(node: Node, source: &[u8], file: &Path, owner_type: &str, fields: &[NamedSlot]) -> Option<MethodDecl> {
    let name_node = node.child_by_field_name("name")?;
    let name = text(name_node, source).to_string();
    let return_type_text = node.child_by_field_name("return_type").map(|t| annotation_type_text(t, source));

    let params = node.child_by_field_name("parameters").map(|p| extract_params(p, source)).unwrap_or_default();

    let mut own_field_accesses = Vec::new();
    let mut foreign_accesses = Vec::new();
    let mut chains = Vec::new();
    let mut trivial_body = false;
    if let Some(body) = node.child_by_field_name("body") {
        walk_accesses(body, source, fields, &params, &mut own_field_accesses, &mut foreign_accesses);
        walk_chains(body, source, &mut chains);
        trivial_body = is_trivial_body(body, source);
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

fn walk_accesses(node: Node, source: &[u8], fields: &[NamedSlot], params: &[NamedSlot], own: &mut Vec<AccessRef>, foreign: &mut Vec<ForeignAccessRef>) {
    match node.kind() {
        "member_expression" => {
            if let (Some(object), Some(property)) = (node.child_by_field_name("object"), node.child_by_field_name("property")) {
                let member_name = text(property, source).to_string();
                let line = line_of(node);
                if object.kind() == "this" {
                    if fields.iter().any(|f| f.name == member_name) {
                        own.push(AccessRef { field_name: member_name, line });
                    }
                } else if object.kind() == "identifier" {
                    let receiver_name = text(object, source).to_string();
                    // A `receiver.method(...)` call's own `member_expression`
                    // is also visited as the `function` of the enclosing
                    // `call_expression` — skip recording it as a plain
                    // field-style access here so the call-expression arm
                    // below records it once, as a call, not twice.
                    let is_call_target = node.parent().is_some_and(|p| p.kind() == "call_expression" && p.child_by_field_name("function").is_some_and(|f| f.id() == node.id()));
                    if !is_call_target {
                        if let Some(param) = params.iter().find(|p| p.name == receiver_name) {
                            foreign.push(ForeignAccessRef { receiver_name, receiver_type: Some(param.type_text.clone()), member_name, line });
                        }
                    }
                }
            }
        }
        "call_expression" => {
            if let Some(function) = node.child_by_field_name("function") {
                if function.kind() == "member_expression" {
                    if let (Some(object), Some(property)) = (function.child_by_field_name("object"), function.child_by_field_name("property")) {
                        if object.kind() == "identifier" {
                            let receiver_name = text(object, source).to_string();
                            if let Some(param) = params.iter().find(|p| p.name == receiver_name) {
                                let member_name = text(property, source).to_string();
                                foreign.push(ForeignAccessRef { receiver_name, receiver_type: Some(param.type_text.clone()), member_name, line: line_of(node) });
                            }
                        }
                        // `this.foo()` (object.kind() == "this") is an
                        // own-method call, not tracked here — same
                        // Feature-Envy scope restriction as `java.rs`.
                    }
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_accesses(child, source, fields, params, own, foreign);
    }
}

/// Finds every maximal call chain in a method body. A `call_expression` is
/// a chain *root* (the outermost link) unless its parent is a
/// `member_expression` that it is the `object` of — in that case it's an
/// inner link of a longer chain already covered when the outer
/// `call_expression` is visited.
fn walk_chains(node: Node, source: &[u8], out: &mut Vec<CallChain>) {
    if node.kind() == "call_expression" {
        let is_inner_link = node
            .parent()
            .filter(|p| p.kind() == "member_expression")
            .and_then(|p| p.child_by_field_name("object"))
            .map(|object| object.id() == node.id())
            .unwrap_or(false);
        if !is_inner_link {
            if let Some(chain) = build_chain(node, source) {
                out.push(chain);
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_chains(child, source, out);
    }
}

/// Unwraps a chain of `call_expression(function: member_expression(object:
/// ...))` links starting at `node` (the outermost call) down to its root
/// expression. Returns `None` for a bare `foo()` call (a plain function
/// call, not a `.member` access shape at all — JS/TS classes have no
/// implicit-`this` call sugar the way Java does, so there's no "this" root
/// to fall back to).
fn build_chain(node: Node, source: &[u8]) -> Option<CallChain> {
    let line = line_of(node);
    let mut member_names_rev = Vec::new();
    let mut current = node;
    loop {
        if current.kind() != "call_expression" {
            member_names_rev.reverse();
            return Some(CallChain { root_text: text(current, source).to_string(), depth: member_names_rev.len(), line, member_names: member_names_rev });
        }
        let function = current.child_by_field_name("function")?;
        if function.kind() != "member_expression" {
            if member_names_rev.is_empty() {
                return None;
            }
            member_names_rev.reverse();
            return Some(CallChain { root_text: text(function, source).to_string(), depth: member_names_rev.len(), line, member_names: member_names_rev });
        }
        let name_text = function.child_by_field_name("property").map(|n| text(n, source).to_string()).unwrap_or_default();
        member_names_rev.push(name_text);
        current = function.child_by_field_name("object")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn parse_ts(source: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        (tree, source.as_bytes().to_vec())
    }

    fn extract(source: &str) -> Vec<TypeDecl> {
        let (tree, bytes) = parse_ts(source);
        extract_types(&tree, &bytes, &PathBuf::from("Sample.ts"))
    }

    fn extract_js(source: &str) -> Vec<TypeDecl> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_javascript::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        extract_types(&tree, source.as_bytes(), &PathBuf::from("Sample.js"))
    }

    #[test]
    fn extracts_a_simple_class_with_fields_and_a_method() {
        let types = extract("class Widget {\n    quantity: number;\n    name: string;\n\n    total(): number {\n        return this.quantity;\n    }\n}\n");
        assert_eq!(types.len(), 1);
        let widget = &types[0];
        assert_eq!(widget.name, "Widget");
        assert_eq!(widget.fields, vec![
            NamedSlot { name: "quantity".into(), type_text: "number".into() },
            NamedSlot { name: "name".into(), type_text: "string".into() },
        ]);
        assert_eq!(widget.methods.len(), 1);
        assert_eq!(widget.methods[0].name, "total");
        assert_eq!(widget.methods[0].own_field_accesses, vec![AccessRef { field_name: "quantity".into(), line: 6 }]);
    }

    #[test]
    fn extracts_an_untyped_javascript_class_with_no_type_text() {
        let types = extract_js("class Widget {\n    constructor() {\n        this.quantity = 0;\n    }\n    total() {\n        return this.quantity;\n    }\n}\n");
        assert_eq!(types.len(), 1);
        // JS has no field-declaration syntax the way TS does — fields
        // assigned only in the constructor aren't picked up by this
        // extractor (same "no scope resolution" limitation as everywhere
        // else in this model); the method itself still extracts cleanly.
        assert_eq!(types[0].methods.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["constructor", "total"]);
    }

    #[test]
    fn classifies_a_parameter_field_access_as_foreign() {
        let types = extract("class W {\n    f(c: Customer): number {\n        return c.balance;\n    }\n}\n");
        let method = &types[0].methods[0];
        assert_eq!(method.foreign_accesses, vec![ForeignAccessRef {
            receiver_name: "c".into(), receiver_type: Some("Customer".into()), member_name: "balance".into(), line: 3,
        }]);
        assert!(method.own_field_accesses.is_empty());
    }

    #[test]
    fn classifies_a_parameter_method_invocation_as_foreign() {
        let types = extract("class W {\n    f(c: Customer): number {\n        return c.getBalance();\n    }\n}\n");
        let method = &types[0].methods[0];
        assert_eq!(method.foreign_accesses, vec![ForeignAccessRef {
            receiver_name: "c".into(), receiver_type: Some("Customer".into()), member_name: "getBalance".into(), line: 3,
        }]);
    }

    #[test]
    fn a_bare_field_reference_without_this_is_not_counted_as_an_own_access() {
        let types = extract("class W {\n    quantity: number;\n    f(): number {\n        return quantity;\n    }\n}\n");
        assert!(types[0].methods[0].own_field_accesses.is_empty());
    }

    #[test]
    fn this_dot_field_matching_no_known_field_is_not_counted() {
        let types = extract("class W {\n    f(): number {\n        return this.nonexistent;\n    }\n}\n");
        assert!(types[0].methods[0].own_field_accesses.is_empty());
    }

    #[test]
    fn a_nested_class_becomes_its_own_type_decl_not_attributed_to_the_outer_class() {
        let types = extract("class Outer {\n    a: number = 0;\n}\nclass Inner {\n    b: number = 0;\n    f(): number { return this.b; }\n}\n");
        assert_eq!(types.len(), 2);
        let outer = types.iter().find(|t| t.name == "Outer").unwrap();
        let inner = types.iter().find(|t| t.name == "Inner").unwrap();
        assert!(outer.methods.is_empty());
        assert_eq!(inner.methods.len(), 1);
        assert_eq!(inner.methods[0].owner_type, "Inner");
    }

    #[test]
    fn method_params_and_return_type_are_captured_as_raw_text() {
        let types = extract("class W {\n    check(x: number, name: string): boolean {\n        return true;\n    }\n}\n");
        let method = &types[0].methods[0];
        assert_eq!(method.params, vec![
            NamedSlot { name: "x".into(), type_text: "number".into() },
            NamedSlot { name: "name".into(), type_text: "string".into() },
        ]);
        assert_eq!(method.return_type_text, Some("boolean".into()));
    }

    #[test]
    fn a_class_with_no_members_extracts_cleanly_with_empty_lists() {
        let types = extract("class Empty {\n}\n");
        assert_eq!(types.len(), 1);
        assert!(types[0].fields.is_empty());
        assert!(types[0].methods.is_empty());
    }

    #[test]
    fn a_message_chain_is_recorded_as_a_single_maximal_chain_not_split_into_shorter_ones() {
        let types = extract("class W {\n    owner: Owner;\n    f(): string {\n        return this.owner.getAddress().getCity().toUpperCase();\n    }\n}\n");
        let chains = &types[0].methods[0].chains;
        assert_eq!(chains.len(), 1, "expected exactly one maximal chain, got {chains:?}");
        assert_eq!(chains[0].depth, 3);
        assert_eq!(chains[0].member_names, vec!["getAddress".to_string(), "getCity".to_string(), "toUpperCase".to_string()]);
    }

    #[test]
    fn a_lone_method_call_is_a_depth_one_chain() {
        let types = extract("class W {\n    f(c: Customer): void {\n        c.getBalance();\n    }\n}\n");
        let chains = &types[0].methods[0].chains;
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].depth, 1);
        assert_eq!(chains[0].root_text, "c");
    }

    #[test]
    fn two_independent_calls_in_the_same_method_produce_two_separate_chains() {
        let types = extract("class W {\n    f(c: Customer, w: Widget): void {\n        c.getBalance();\n        w.getQuantity();\n    }\n}\n");
        assert_eq!(types[0].methods[0].chains.len(), 2);
    }

    #[test]
    fn a_bare_function_call_is_not_recorded_as_a_chain() {
        let types = extract("class W {\n    f(): void {\n        doSomething();\n    }\n}\n");
        assert!(types[0].methods[0].chains.is_empty());
    }

    #[test]
    fn extracts_the_superclass_name() {
        let types = extract("class Sub extends Base {\n}\n");
        assert_eq!(types[0].superclass.as_deref(), Some("Base"));
    }

    #[test]
    fn a_class_with_no_extends_clause_has_no_superclass() {
        let types = extract("class Standalone {\n}\n");
        assert_eq!(types[0].superclass, None);
    }

    #[test]
    fn an_empty_method_body_is_a_trivial_body() {
        let types = extract("class Sub extends Base {\n    save(): void {\n    }\n}\n");
        assert!(types[0].methods[0].is_trivial_body);
    }

    #[test]
    fn a_lone_not_implemented_throw_is_a_trivial_body() {
        let types = extract("class Sub extends Base {\n    save(): void {\n        throw new Error(\"not implemented\");\n    }\n}\n");
        assert!(types[0].methods[0].is_trivial_body);
    }

    #[test]
    fn a_real_method_body_is_not_trivial() {
        let types = extract("class Sub extends Base {\n    save(): void {\n        this.persist();\n    }\n}\n");
        assert!(!types[0].methods[0].is_trivial_body);
    }
}
