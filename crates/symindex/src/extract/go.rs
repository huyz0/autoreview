//! Go struct/field/method extraction via a real tree-sitter parse. Grammar
//! node kinds verified empirically the same way as `java.rs` — via
//! `ast-grep run --lang go -p "<source>" file.go --debug-query=ast` against
//! hand-written fixtures.
//!
//! Go has no fixed `this`/`self` keyword — a method's receiver binds its
//! own chosen name (e.g. `w` in `func (w *Widget) Foo()`), so own-vs-foreign
//! access classification must track that per-method bound name rather than
//! matching a keyword, unlike Java's `this`. An unnamed receiver
//! (`func (Widget) Foo()`, legal Go) means no bound name exists at all —
//! own-access classification is skipped entirely for such methods (safe,
//! conservative: nothing is misclassified, some own-access just isn't
//! counted).
//!
//! Plain functions (`function_declaration`, no receiver) aren't attached to
//! any `TypeDecl` and are skipped in this pass — a real, stated gap: Go's
//! dominant "envy" shape is often a free function reaching into a struct's
//! fields, not a method on a different struct. Left as documented future
//! work, not solved here.

use std::path::Path;

use tree_sitter::Node;

use crate::model::{AccessRef, ForeignAccessRef, MethodDecl, NamedSlot, TypeDecl};

fn text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn line_of(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

/// Strips a leading `*` so a pointer-receiver's type text (`*Widget`)
/// matches the struct name (`Widget`) it declares fields for.
fn strip_pointer(type_text: &str) -> &str {
    type_text.trim_start_matches('*')
}

pub fn extract_types(tree: &tree_sitter::Tree, source: &[u8], file: &Path) -> Vec<TypeDecl> {
    let root = tree.root_node();

    let mut structs: Vec<TypeDecl> = Vec::new();
    collect_structs(root, source, file, &mut structs);

    let mut methods_by_owner: Vec<(String, MethodDecl)> = Vec::new();
    collect_methods(root, source, file, &mut methods_by_owner);

    for (owner, mut method) in methods_by_owner {
        if let Some(type_decl) = structs.iter_mut().find(|t| t.name == owner) {
            // `walk_accesses` only had the receiver's bound name in scope,
            // not the struct's actual field list (methods are collected in
            // a separate pass from structs) — filter own-field accesses
            // down to real fields now that both are known, same
            // field-name validation `java.rs` applies inline.
            method.own_field_accesses.retain(|access| type_decl.fields.iter().any(|f| f.name == access.field_name));
            type_decl.methods.push(method);
        }
        // A method whose receiver type isn't a struct found in this file
        // (e.g. defined elsewhere) is silently dropped — cross-file
        // struct/method attachment isn't attempted in this pass.
    }

    structs
}

fn collect_structs(node: Node, source: &[u8], file: &Path, out: &mut Vec<TypeDecl>) {
    if node.kind() == "type_declaration" {
        let mut cursor = node.walk();
        for spec in node.named_children(&mut cursor) {
            if spec.kind() == "type_spec" {
                if let Some(type_decl) = extract_struct(spec, source, file) {
                    out.push(type_decl);
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_structs(child, source, file, out);
    }
}

fn extract_struct(spec: Node, source: &[u8], file: &Path) -> Option<TypeDecl> {
    let name_node = spec.child_by_field_name("name")?;
    let type_node = spec.child_by_field_name("type")?;
    if type_node.kind() != "struct_type" {
        return None;
    }
    let name = text(name_node, source).to_string();

    let mut fields = Vec::new();
    if let Some(field_list) = type_node.named_child(0) {
        let mut cursor = field_list.walk();
        for decl in field_list.named_children(&mut cursor) {
            if decl.kind() == "field_declaration" {
                let type_text = decl.child_by_field_name("type").map(|t| text(t, source).to_string()).unwrap_or_default();
                let mut nc = decl.walk();
                for name_field in decl.children_by_field_name("name", &mut nc) {
                    fields.push(NamedSlot { name: text(name_field, source).to_string(), type_text: type_text.clone() });
                }
            }
        }
    }

    Some(TypeDecl { name, file: file.to_path_buf(), start_line: line_of(spec), fields, methods: Vec::new() })
}

fn collect_methods(node: Node, source: &[u8], file: &Path, out: &mut Vec<(String, MethodDecl)>) {
    if node.kind() == "method_declaration" {
        if let Some((owner, method)) = extract_method(node, source, file) {
            out.push((owner, method));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_methods(child, source, file, out);
    }
}

fn extract_method(node: Node, source: &[u8], file: &Path) -> Option<(String, MethodDecl)> {
    let receiver_list = node.child_by_field_name("receiver")?;
    let receiver_decl = receiver_list.named_child(0)?;
    let receiver_type_node = receiver_decl.child_by_field_name("type")?;
    let owner_type = strip_pointer(text(receiver_type_node, source)).to_string();
    // An unnamed receiver (`func (Widget) Foo()`) has no `name` field.
    let receiver_name = receiver_decl.child_by_field_name("name").map(|n| text(n, source).to_string());

    let name_node = node.child_by_field_name("name")?;
    let name = text(name_node, source).to_string();
    let return_type_text = node.child_by_field_name("result").map(|t| text(t, source).to_string());

    let mut params = Vec::new();
    if let Some(params_node) = node.child_by_field_name("parameters") {
        let mut cursor = params_node.walk();
        for p in params_node.named_children(&mut cursor) {
            if p.kind() == "parameter_declaration" {
                let type_text = p.child_by_field_name("type").map(|t| text(t, source).to_string()).unwrap_or_default();
                if let Some(param_name) = p.child_by_field_name("name") {
                    params.push(NamedSlot { name: text(param_name, source).to_string(), type_text });
                }
            }
        }
    }

    // own_field_accesses is provisional here — every selector matching the
    // receiver's bound name is recorded; extract_types filters it down to
    // real fields once the struct's field list is known (methods and
    // structs are collected in separate passes, joined by owner-type name).
    let mut own_field_accesses = Vec::new();
    let mut foreign_accesses = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        walk_accesses(body, source, receiver_name.as_deref(), &params, &mut own_field_accesses, &mut foreign_accesses);
    }

    Some((
        owner_type.clone(),
        MethodDecl {
            name,
            owner_type,
            file: file.to_path_buf(),
            start_line: line_of(node),
            end_line: node.end_position().row as u32 + 1,
            params,
            return_type_text,
            own_field_accesses,
            foreign_accesses,
            chains: Vec::new(),
        },
    ))
}

/// Walks a method body classifying `selector_expression` nodes
/// (`operand.field`) as an own access (operand text == the receiver's own
/// bound name) or a foreign access (operand matches a non-receiver
/// parameter). Field-name validation against the struct's actual field
/// list happens after the fact in `extract_types` (own accesses are
/// filtered there), since Go's single-pass walk here doesn't have the
/// struct's field list in scope — methods are collected in a separate pass
/// from structs and only joined by owner-type name afterward.
fn walk_accesses(node: Node, source: &[u8], receiver_name: Option<&str>, params: &[NamedSlot], own: &mut Vec<AccessRef>, foreign: &mut Vec<ForeignAccessRef>) {
    if node.kind() == "selector_expression" {
        if let (Some(operand), Some(field)) = (node.child_by_field_name("operand"), node.child_by_field_name("field")) {
            if operand.kind() == "identifier" {
                let operand_text = text(operand, source);
                let field_name = text(field, source).to_string();
                let line = line_of(node);
                if Some(operand_text) == receiver_name {
                    own.push(AccessRef { field_name, line });
                } else if let Some(param) = params.iter().find(|p| p.name == operand_text) {
                    foreign.push(ForeignAccessRef { receiver_name: operand_text.to_string(), receiver_type: Some(param.type_text.clone()), member_name: field_name, line });
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_accesses(child, source, receiver_name, params, own, foreign);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn extract(source: &str) -> Vec<TypeDecl> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_go::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        extract_types(&tree, source.as_bytes(), &PathBuf::from("sample.go"))
    }

    #[test]
    fn extracts_a_struct_with_fields_and_a_pointer_receiver_method() {
        let types = extract("package main\n\ntype Widget struct {\n\tQuantity int\n\tName     string\n}\n\nfunc (w *Widget) Total() int {\n\treturn w.Quantity\n}\n");
        assert_eq!(types.len(), 1);
        let widget = &types[0];
        assert_eq!(widget.name, "Widget");
        assert_eq!(widget.fields, vec![
            NamedSlot { name: "Quantity".into(), type_text: "int".into() },
            NamedSlot { name: "Name".into(), type_text: "string".into() },
        ]);
        assert_eq!(widget.methods.len(), 1);
        assert_eq!(widget.methods[0].own_field_accesses, vec![AccessRef { field_name: "Quantity".into(), line: 9 }]);
    }

    #[test]
    fn a_multi_name_field_declaration_yields_one_slot_per_name() {
        let types = extract("package main\n\ntype Point struct {\n\tX, Y int\n}\n");
        assert_eq!(types[0].fields, vec![
            NamedSlot { name: "X".into(), type_text: "int".into() },
            NamedSlot { name: "Y".into(), type_text: "int".into() },
        ]);
    }

    #[test]
    fn classifies_a_parameter_field_access_as_foreign() {
        let types = extract("package main\n\ntype Widget struct {\n\tQuantity int\n}\n\nfunc (w *Widget) Combine(c *Customer) int {\n\treturn c.Balance\n}\n");
        let method = &types[0].methods[0];
        assert_eq!(method.foreign_accesses, vec![ForeignAccessRef {
            receiver_name: "c".into(), receiver_type: Some("*Customer".into()), member_name: "Balance".into(), line: 8,
        }]);
    }

    #[test]
    fn a_value_receiver_without_a_pointer_resolves_the_owner_type_directly() {
        let types = extract("package main\n\ntype Widget struct {\n\tX int\n}\n\nfunc (w Widget) Get() int {\n\treturn w.X\n}\n");
        assert_eq!(types[0].name, "Widget");
        assert_eq!(types[0].methods[0].owner_type, "Widget");
    }

    #[test]
    fn an_unnamed_receiver_has_no_own_field_accesses_but_still_extracts_cleanly() {
        let types = extract("package main\n\ntype Widget struct {\n\tX int\n}\n\nfunc (Widget) NoName() {}\n");
        assert_eq!(types[0].methods.len(), 1);
        assert!(types[0].methods[0].own_field_accesses.is_empty());
    }

    #[test]
    fn a_receiver_selector_that_is_not_a_real_field_is_not_counted_as_an_own_access() {
        let types = extract("package main\n\ntype Widget struct {\n\tX int\n}\n\nfunc (w *Widget) Helper() int {\n\treturn w.NotAField\n}\n");
        assert!(types[0].methods[0].own_field_accesses.is_empty());
    }

    #[test]
    fn a_method_on_an_undeclared_type_in_this_file_is_silently_dropped() {
        let types = extract("package main\n\nfunc (w *Elsewhere) Foo() {}\n");
        assert!(types.is_empty());
    }

    #[test]
    fn method_params_and_return_type_are_captured_as_raw_text() {
        let types = extract("package main\n\ntype W struct{}\n\nfunc (w *W) Check(x int, name string) bool {\n\treturn true\n}\n");
        let method = &types[0].methods[0];
        assert_eq!(method.params, vec![
            NamedSlot { name: "x".into(), type_text: "int".into() },
            NamedSlot { name: "name".into(), type_text: "string".into() },
        ]);
        assert_eq!(method.return_type_text, Some("bool".into()));
    }

    #[test]
    fn a_plain_function_with_no_receiver_is_not_attached_to_any_type() {
        let types = extract("package main\n\ntype W struct{ X int }\n\nfunc Standalone() int { return 1 }\n");
        assert_eq!(types.len(), 1);
        assert!(types[0].methods.is_empty());
    }
}
