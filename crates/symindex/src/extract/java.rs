//! Java class/field/method extraction via a real tree-sitter parse (not a
//! line-scan — see the crate's own module docs for why `complexity.rs`'s
//! brace-counting approach can't safely do this). Grammar node kinds below
//! were verified empirically against the real `tree-sitter-java` grammar
//! (via `ast-grep run --lang java -p "<source>" file.java --debug-query=ast`
//! against hand-written fixtures), not assumed from documentation.
//!
//! Known, deliberate limitation: own-field access is only recognized via
//! explicit `this.field` — a bare `field` reference (the Java-idiomatic
//! style when there's no name collision) is NOT counted as an own access,
//! since a bare identifier is structurally indistinguishable from a param
//! or local reference without real scope resolution. This under-counts
//! `own_field_accesses`, which is the conservative direction (fewer false
//! "envy" positives from methods that never reference `this.` explicitly),
//! consistent with this project's precision-over-recall bias — but it
//! means Feature Envy (a later phase) may need this revisited once real
//! false-positive/false-negative rates are observed.

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
/// nesting depth — each becomes its own `TypeDecl`; a nested/inner class's
/// methods are attributed to it, not to the outer class, since only its
/// own immediate `class_body` children are inspected for fields/methods.
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

fn extract_class(node: Node, source: &[u8], file: &Path) -> Option<TypeDecl> {
    let name_node = node.child_by_field_name("name")?;
    let name = text(name_node, source).to_string();
    let body = node.child_by_field_name("body")?;

    // Fields first, in a separate pass, so a method declared before a field
    // in source order (legal in Java) still sees the complete field list
    // when classifying its own-field accesses below.
    let mut fields = Vec::new();
    {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if child.kind() == "field_declaration" {
                let type_text = child.child_by_field_name("type").map(|t| text(t, source).to_string()).unwrap_or_default();
                let mut dc = child.walk();
                for declarator in child.children_by_field_name("declarator", &mut dc) {
                    if let Some(name_node) = declarator.child_by_field_name("name") {
                        fields.push(NamedSlot { name: text(name_node, source).to_string(), type_text: type_text.clone() });
                    }
                }
            }
        }
    }

    let mut methods = Vec::new();
    {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if child.kind() == "method_declaration" {
                if let Some(m) = extract_method(child, source, file, &name, &fields) {
                    methods.push(m);
                }
            }
        }
    }

    Some(TypeDecl { name, file: file.to_path_buf(), start_line: line_of(node), fields, methods })
}

fn extract_method(node: Node, source: &[u8], file: &Path, owner_type: &str, fields: &[NamedSlot]) -> Option<MethodDecl> {
    let name_node = node.child_by_field_name("name")?;
    let name = text(name_node, source).to_string();
    let return_type_text = node.child_by_field_name("type").map(|t| text(t, source).to_string());

    let mut params = Vec::new();
    if let Some(params_node) = node.child_by_field_name("parameters") {
        let mut cursor = params_node.walk();
        for p in params_node.named_children(&mut cursor) {
            if p.kind() == "formal_parameter" {
                let type_text = p.child_by_field_name("type").map(|t| text(t, source).to_string()).unwrap_or_default();
                if let Some(param_name) = p.child_by_field_name("name") {
                    params.push(NamedSlot { name: text(param_name, source).to_string(), type_text });
                }
            }
        }
    }

    let mut own_field_accesses = Vec::new();
    let mut foreign_accesses = Vec::new();
    let mut chains = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        walk_accesses(body, source, fields, &params, &mut own_field_accesses, &mut foreign_accesses);
        walk_chains(body, source, &mut chains);
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
    })
}

fn walk_accesses(node: Node, source: &[u8], fields: &[NamedSlot], params: &[NamedSlot], own: &mut Vec<AccessRef>, foreign: &mut Vec<ForeignAccessRef>) {
    match node.kind() {
        "field_access" => {
            if let (Some(object), Some(field)) = (node.child_by_field_name("object"), node.child_by_field_name("field")) {
                let field_name = text(field, source).to_string();
                let line = line_of(node);
                if object.kind() == "this" {
                    if fields.iter().any(|f| f.name == field_name) {
                        own.push(AccessRef { field_name, line });
                    }
                } else if object.kind() == "identifier" {
                    let receiver_name = text(object, source).to_string();
                    if let Some(param) = params.iter().find(|p| p.name == receiver_name) {
                        foreign.push(ForeignAccessRef { receiver_name, receiver_type: Some(param.type_text.clone()), member_name: field_name, line });
                    }
                }
            }
        }
        "method_invocation" => {
            if let (Some(object), Some(name_node)) = (node.child_by_field_name("object"), node.child_by_field_name("name")) {
                if object.kind() == "identifier" {
                    let receiver_name = text(object, source).to_string();
                    if let Some(param) = params.iter().find(|p| p.name == receiver_name) {
                        let member_name = text(name_node, source).to_string();
                        foreign.push(ForeignAccessRef { receiver_name, receiver_type: Some(param.type_text.clone()), member_name, line: line_of(node) });
                    }
                }
                // `this.foo()` (object.kind() == "this") is an own-method call,
                // not tracked here — Feature Envy only weighs own-FIELD
                // accesses against foreign accesses, per the model's own docs.
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_accesses(child, source, fields, params, own, foreign);
    }
}

/// Finds every maximal call chain in a method body: a `method_invocation`
/// node is a chain *root* (the outermost link) unless its parent is itself
/// a `method_invocation` whose `object` is this node — in that case it's an
/// inner link of a longer chain already covered when the outer node is
/// visited, so it's skipped here to avoid double-counting the same chain
/// as multiple shorter ones.
fn walk_chains(node: Node, source: &[u8], out: &mut Vec<CallChain>) {
    if node.kind() == "method_invocation" {
        let is_inner_link = node
            .parent()
            .and_then(|p| if p.kind() == "method_invocation" { p.child_by_field_name("object") } else { None })
            .map(|object| object.id() == node.id())
            .unwrap_or(false);
        if !is_inner_link {
            out.push(build_chain(node, source));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_chains(child, source, out);
    }
}

/// Unwraps a chain of `method_invocation.object` links starting at `node`
/// (the outermost call) down to its root expression (an identifier,
/// `this`, or anything else that isn't itself a call), collecting member
/// names in root-to-tip order.
fn build_chain(node: Node, source: &[u8]) -> CallChain {
    let line = line_of(node);
    let mut member_names_rev = Vec::new();
    let mut current = node;
    loop {
        if current.kind() != "method_invocation" {
            member_names_rev.reverse();
            return CallChain { root_text: text(current, source).to_string(), depth: member_names_rev.len(), line, member_names: member_names_rev };
        }
        let name_text = current.child_by_field_name("name").map(|n| text(n, source).to_string()).unwrap_or_default();
        member_names_rev.push(name_text);
        match current.child_by_field_name("object") {
            Some(object) => current = object,
            None => {
                // An implicit-`this` call with no explicit object, e.g. a
                // bare `foo()` invoking a method on the enclosing instance.
                member_names_rev.reverse();
                return CallChain { root_text: "this".to_string(), depth: member_names_rev.len(), line, member_names: member_names_rev };
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
        parser.set_language(&tree_sitter_java::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        (tree, source.as_bytes().to_vec())
    }

    fn extract(source: &str) -> Vec<TypeDecl> {
        let (tree, bytes) = parse(source);
        extract_types(&tree, &bytes, &PathBuf::from("Sample.java"))
    }

    #[test]
    fn extracts_a_simple_class_with_fields_and_a_method() {
        let types = extract(
            "class Widget {\n    int quantity;\n    String name;\n\n    int total() {\n        return this.quantity;\n    }\n}\n",
        );
        assert_eq!(types.len(), 1);
        let widget = &types[0];
        assert_eq!(widget.name, "Widget");
        assert_eq!(widget.fields, vec![
            NamedSlot { name: "quantity".into(), type_text: "int".into() },
            NamedSlot { name: "name".into(), type_text: "String".into() },
        ]);
        assert_eq!(widget.methods.len(), 1);
        assert_eq!(widget.methods[0].name, "total");
        assert_eq!(widget.methods[0].own_field_accesses, vec![AccessRef { field_name: "quantity".into(), line: 6 }]);
    }

    #[test]
    fn a_multi_name_field_declaration_yields_one_slot_per_name() {
        let types = extract("class W {\n    String name, label;\n}\n");
        assert_eq!(types[0].fields, vec![
            NamedSlot { name: "name".into(), type_text: "String".into() },
            NamedSlot { name: "label".into(), type_text: "String".into() },
        ]);
    }

    #[test]
    fn classifies_a_parameter_field_access_as_foreign() {
        let types = extract("class W {\n    void f(Customer c) {\n        int x = c.balance;\n    }\n}\n");
        let method = &types[0].methods[0];
        assert_eq!(method.foreign_accesses, vec![ForeignAccessRef {
            receiver_name: "c".into(), receiver_type: Some("Customer".into()), member_name: "balance".into(), line: 3,
        }]);
        assert!(method.own_field_accesses.is_empty());
    }

    #[test]
    fn classifies_a_parameter_method_invocation_as_foreign() {
        let types = extract("class W {\n    void f(Customer c) {\n        int x = c.getBalance();\n    }\n}\n");
        let method = &types[0].methods[0];
        assert_eq!(method.foreign_accesses, vec![ForeignAccessRef {
            receiver_name: "c".into(), receiver_type: Some("Customer".into()), member_name: "getBalance".into(), line: 3,
        }]);
    }

    #[test]
    fn a_bare_field_reference_without_this_is_not_counted_as_an_own_access() {
        // Documented limitation (see module docs): bare identifiers aren't
        // scope-resolved, so this stays conservative rather than guessing.
        let types = extract("class W {\n    int quantity;\n    int f() {\n        return quantity;\n    }\n}\n");
        assert!(types[0].methods[0].own_field_accesses.is_empty());
    }

    #[test]
    fn this_dot_field_matching_no_known_field_is_not_counted() {
        let types = extract("class W {\n    int f() {\n        return this.nonexistent;\n    }\n}\n");
        assert!(types[0].methods[0].own_field_accesses.is_empty());
    }

    #[test]
    fn a_nested_class_becomes_its_own_type_decl_not_attributed_to_the_outer_class() {
        let types = extract("class Outer {\n    int a;\n    class Inner {\n        int b;\n        int f() { return this.b; }\n    }\n}\n");
        assert_eq!(types.len(), 2);
        let outer = types.iter().find(|t| t.name == "Outer").unwrap();
        let inner = types.iter().find(|t| t.name == "Inner").unwrap();
        assert!(outer.methods.is_empty());
        assert_eq!(inner.methods.len(), 1);
        assert_eq!(inner.methods[0].owner_type, "Inner");
    }

    #[test]
    fn method_params_and_return_type_are_captured_as_raw_text() {
        let types = extract("class W {\n    boolean check(int x, String name) {\n        return true;\n    }\n}\n");
        let method = &types[0].methods[0];
        assert_eq!(method.params, vec![
            NamedSlot { name: "x".into(), type_text: "int".into() },
            NamedSlot { name: "name".into(), type_text: "String".into() },
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
        let types = extract("class W {\n    Owner owner;\n    String f() {\n        return owner.getAddress().getCity().toUpperCase();\n    }\n}\n");
        let chains = &types[0].methods[0].chains;
        assert_eq!(chains.len(), 1, "expected exactly one maximal chain, got {chains:?}");
        assert_eq!(chains[0].root_text, "owner");
        assert_eq!(chains[0].depth, 3);
        assert_eq!(chains[0].member_names, vec!["getAddress".to_string(), "getCity".to_string(), "toUpperCase".to_string()]);
    }

    #[test]
    fn a_lone_method_call_is_a_depth_one_chain() {
        let types = extract("class W {\n    void f(Customer c) {\n        c.getBalance();\n    }\n}\n");
        let chains = &types[0].methods[0].chains;
        assert_eq!(chains.len(), 1);
        assert_eq!(chains[0].depth, 1);
        assert_eq!(chains[0].root_text, "c");
    }

    #[test]
    fn two_independent_calls_in_the_same_method_produce_two_separate_chains() {
        let types = extract("class W {\n    void f(Customer c, Widget w) {\n        c.getBalance();\n        w.getQuantity();\n    }\n}\n");
        assert_eq!(types[0].methods[0].chains.len(), 2);
    }
}
