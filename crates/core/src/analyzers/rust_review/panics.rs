use autoreview_schema::Severity;
use tree_sitter::Node;

use super::syntax::{attributes, children, is_comment, Source};
use super::{Review, RustReviewRule};

const DROP_PANIC: RustReviewRule = RustReviewRule {
    id: "rust-unconditional-drop-panic",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Unconditional panic in Drop",
    message: "This destructor unconditionally panics. A panic during unwinding can abort the process. Report cleanup errors through an explicit fallible method.",
    reference: "https://doc.rust-lang.org/std/ops/trait.Drop.html#panics",
    cwe: None,
};

const FFI_PANIC: RustReviewRule = RustReviewRule {
    id: "rust-unconditional-ffi-panic",
    category: "correctness",
    severity: Severity::Medium,
    semantic: true,
    title: "Unconditional panic across an FFI boundary",
    message: "This exported function unconditionally panics, which aborts the process at its non-unwinding ABI boundary. Return an error through the foreign interface.",
    reference: "https://doc.rust-lang.org/nomicon/ffi.html#ffi-and-unwinding",
    cwe: None,
};

pub(super) const RULES: &[&RustReviewRule] = &[&DROP_PANIC, &FFI_PANIC];

pub(super) fn check<'tree>(review: &mut Review<'tree, '_>, function: Node<'tree>) {
    if function.kind() != "function_item" {
        return;
    }
    let (rule, related) = if let Some(trait_name) = drop_trait(&review.source, function) {
        (&DROP_PANIC, vec![trait_name])
    } else {
        let Some(abi) = non_unwinding_abi(&review.source, function) else {
            return;
        };
        let Some(export) =
            attributes(function).find(|attribute| is_export(&review.source, *attribute))
        else {
            return;
        };
        (&FFI_PANIC, vec![abi, export])
    };
    if let Some(panic) = sole_standard_panic(&review.source, function) {
        review.report_related(rule, panic, &related);
    }
}

fn sole_standard_panic<'tree>(
    source: &Source<'tree, '_>,
    function: Node<'tree>,
) -> Option<Node<'tree>> {
    let mut body =
        children(function.child_by_field_name("body")?).filter(|item| !is_comment(*item));
    let statement = body.next()?;
    if body.next().is_some() {
        return None;
    }
    let invocation = if statement.kind() == "expression_statement" {
        children(statement).find(|item| !is_comment(*item))?
    } else {
        statement
    };
    if invocation.kind() != "macro_invocation"
        || !source.standard_macro(
            invocation.child_by_field_name("macro")?,
            &["panic", "todo", "unimplemented"],
        )
    {
        return None;
    }
    let arguments = children(invocation).find(|node| node.kind() == "token_tree")?;
    // An argument expression can diverge before the panic macro takes effect.
    match inner_tokens(arguments).as_slice() {
        [] => Some(invocation),
        [message] if is_string(*message) => Some(invocation),
        [message, comma] if is_string(*message) && comma.kind() == "," => Some(invocation),
        _ => None,
    }
}

fn drop_trait<'tree>(source: &Source<'tree, '_>, function: Node<'tree>) -> Option<Node<'tree>> {
    let name = source.text(function.child_by_field_name("name")?);
    if !matches!(name, "drop" | "r#drop") {
        return None;
    }
    let parent = function.parent()?;
    let implementation = parent.parent()?;
    if parent.kind() != "declaration_list" || implementation.kind() != "impl_item" {
        return None;
    }
    let trait_name = implementation.child_by_field_name("trait")?;
    (source.unshadowed_prelude(trait_name, "Drop")
        || matches!(
            source.external_path(trait_name).as_deref(),
            Some("std::ops::Drop" | "core::ops::Drop")
        ))
    .then_some(trait_name)
}

fn non_unwinding_abi<'tree>(
    source: &Source<'tree, '_>,
    function: Node<'tree>,
) -> Option<Node<'tree>> {
    let modifiers = children(function).find(|child| child.kind() == "function_modifiers")?;
    let abi = children(modifiers).find(|child| child.kind() == "extern_modifier")?;
    children(abi)
        .find(|child| !is_comment(*child))
        .is_none_or(|name| matches!(source.text(name), "\"C\"" | "\"system\""))
        .then_some(abi)
}

fn is_export(source: &Source<'_, '_>, attribute: Node<'_>) -> bool {
    let Some(name) = children(attribute).find(|child| !is_comment(*child)) else {
        return false;
    };
    let arguments = attribute.child_by_field_name("arguments");
    let value = attribute.child_by_field_name("value");
    match source.text(name) {
        "no_mangle" => arguments.is_none() && value.is_none(),
        "export_name" => arguments.is_none() && value.is_some_and(is_string),
        "unsafe" => arguments.is_some_and(|arguments| {
            if arguments.child(0).is_none_or(|open| open.kind() != "(") {
                return false;
            }
            match inner_tokens(arguments).as_slice() {
                [name] => source.text(*name) == "no_mangle",
                [name, equals, value] => {
                    source.text(*name) == "export_name" && equals.kind() == "=" && is_string(*value)
                }
                _ => false,
            }
        }),
        _ => false,
    }
}

fn inner_tokens(token_tree: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = token_tree.walk();
    token_tree
        .children(&mut cursor)
        .filter(|child| {
            !is_comment(*child) && !matches!(child.kind(), "(" | ")" | "[" | "]" | "{" | "}")
        })
        .collect()
}

fn is_string(node: Node<'_>) -> bool {
    matches!(node.kind(), "string_literal" | "raw_string_literal")
}

#[cfg(test)]
mod tests {
    use super::super::analyze_file;

    #[test]
    fn flags_sole_standard_panics_in_bare_qualified_and_imported_drop_impls() {
        let findings = analyze_file(
            "lib.rs",
            r#"
use std::ops::Drop as Destructor;
struct Bare;
impl Drop for Bare {
    fn drop(&mut self) { /* cleanup */ panic!(); }
}
struct Qualified;
impl ::core::ops::Drop for Qualified {
    fn drop(&mut self) { ::std::todo!("implement cleanup",) }
}
struct Imported;
impl Destructor for Imported {
    fn drop(&mut self) { unimplemented!(r"cleanup"); }
}
"#,
        );
        assert_eq!(findings.len(), 3, "{findings:#?}");
        assert!(findings.iter().all(|finding| {
            finding.source.rule_id.as_deref() == Some("rust-unconditional-drop-panic")
                && finding
                    .related_locations
                    .as_ref()
                    .is_some_and(|locations| locations.len() == 1)
        }));
    }

    #[test]
    fn flags_exported_c_system_and_implicit_c_abi_panics() {
        let findings = analyze_file(
            "lib.rs",
            r#"
#[unsafe(no_mangle)]
#[inline(never)]
extern "C" fn c() { panic!(); }
#[unsafe(export_name = "worker")]
extern "system" fn worker() { core::todo!("implement worker") }
#[export_name = "legacy"]
pub extern fn legacy() { std::unimplemented!("implement legacy"); }
"#,
        );
        assert_eq!(findings.len(), 3, "{findings:#?}");
        assert!(findings.iter().all(|finding| {
            finding.source.rule_id.as_deref() == Some("rust-unconditional-ffi-panic")
                && finding
                    .related_locations
                    .as_ref()
                    .is_some_and(|locations| locations.len() == 2)
        }));
    }

    #[test]
    fn skips_shadowed_macros_and_traits_and_panics_outside_the_proven_shapes() {
        let findings = analyze_file(
            "lib.rs",
            r#"
mod shadowed_panic {
    macro_rules! panic { () => { () } }
    struct Item;
    impl Drop for Item {
        fn drop(&mut self) { panic!(); }
    }
    #[unsafe(no_mangle)]
    extern "C" fn custom() { panic!(); }
}
mod shadowed_drop {
    trait Drop { fn drop(&mut self); }
    struct Item;
    impl Drop for Item {
        fn drop(&mut self) { std::panic!("custom trait"); }
    }
}
struct Inherent;
impl Inherent {
    fn drop(&mut self) { panic!(); }
}
struct Multiple;
impl Drop for Multiple {
    fn drop(&mut self) { before(); panic!(); }
}
#[unsafe(no_mangle)]
extern "C" fn caught() { std::panic::catch_unwind(|| panic!()); }
#[unsafe(no_mangle)]
extern "C-unwind" fn unwind() { panic!(); }
#[unsafe(no_mangle)]
extern "C" fn argument_returns() { panic!("{}", return); }
extern "C" fn unexported() { std::panic!(); }
#[cfg_attr(test, unsafe(no_mangle))]
extern "C" fn conditionally_exported() { std::panic!(); }
"#,
        );
        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[test]
    fn skips_a_bare_macro_shadowed_by_a_nested_export() {
        let findings = analyze_file(
            "lib.rs",
            r#"
mod macros {
    #[macro_export]
    macro_rules! panic { () => { () }; }
}
#[unsafe(no_mangle)]
extern "C" fn custom() { panic!(); }
"#,
        );
        assert!(findings.is_empty(), "{findings:#?}");
    }
}
