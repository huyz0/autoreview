use std::collections::HashMap;
use std::path::Path;

use crate::extract::java::bequest::{JavaClass, JavaParameter, JavaScope};
use crate::model::{SymbolIndex, TypeDecl};

use super::RefusedBequestFinding;

type Types<'a> = HashMap<String, Option<&'a TypeDecl>>;

pub(super) fn is_java(file: &Path) -> bool {
    file.extension().is_some_and(|extension| extension == "java")
}

pub(super) fn find(index: &SymbolIndex) -> Vec<RefusedBequestFinding> {
    let mut types: Types<'_> = HashMap::new();
    for ty in &index.types {
        let Some(java) = &ty.java else { continue };
        let name = qualify(&java.scope.package, &ty.name);
        types.entry(name).and_modify(|entry| *entry = None).or_insert(Some(ty));
    }

    let mut findings = Vec::new();
    for child in &index.types {
        let Some(java) = &child.java else { continue };
        let Some(parent) = resolve_parent(child, java, &types) else { continue };
        let Some(parent_java) = &parent.java else { continue };
        collect_overrides(child, java, parent, parent_java, &types, &mut findings);
    }
    findings
}

fn resolve_parent<'a>(child: &TypeDecl, java: &JavaClass, types: &Types<'a>) -> Option<&'a TypeDecl> {
    let name = resolve_name(java.parent.as_deref()?, &java.scope, types)?;
    let parent = types.get(&name).copied().flatten()?;
    let parent_java = parent.java.as_ref()?;
    (!std::ptr::eq(child, parent) && (parent_java.public || parent_java.scope.package == java.scope.package))
        .then_some(parent)
}

fn collect_overrides(
    child: &TypeDecl,
    java: &JavaClass,
    parent: &TypeDecl,
    parent_java: &JavaClass,
    types: &Types<'_>,
    findings: &mut Vec<RefusedBequestFinding>,
) {
    let same_package = java.scope.package == parent_java.scope.package;
    let mut inherited = HashMap::new();
    for (method, signature) in parent.methods.iter().zip(&parent_java.signatures) {
        let Some(signature) = signature.as_ref().filter(|s| !s.is_final && (same_package || !s.package_private)) else { continue };
        if let Some(parameters) = resolved_parameters(&signature.parameters, parent_java, None, types) {
            inherited.entry(method.name.as_str()).or_insert_with(Vec::new).push(parameters);
        }
    }

    for (method, signature) in child.methods.iter().zip(&java.signatures) {
        let Some(signature) = signature.as_ref().filter(|_| method.is_trivial_body) else { continue };
        let Some(candidates) = inherited.get(method.name.as_str()) else { continue };
        let Some(parameters) = resolved_parameters(&signature.parameters, java, Some(parent_java), types) else { continue };
        if candidates.contains(&parameters) {
            findings.push(RefusedBequestFinding {
                file: method.file.clone(),
                line: method.start_line,
                owner_type: child.name.clone(),
                superclass: parent.name.clone(),
                method: method.name.clone(),
            });
        }
    }
}

fn resolved_parameters(params: &[JavaParameter], class: &JavaClass, parent: Option<&JavaClass>, types: &Types<'_>) -> Option<Vec<JavaParameter>> {
    params.iter().map(|param| resolve_parameter(param, class, parent, types)).collect()
}

fn resolve_parameter(param: &JavaParameter, class: &JavaClass, parent: Option<&JavaClass>, types: &Types<'_>) -> Option<JavaParameter> {
    let head = param.name.split('.').next()?;
    let shadows = |ty: &JavaClass| ty.member_types.iter().any(|name| name == head);
    if shadows(class) || parent.is_some_and(shadows) {
        return None;
    }
    let name = if param.primitive { param.name.clone() } else { resolve_name(&param.name, &class.scope, types)? };
    Some(JavaParameter {
        name,
        primitive: param.primitive,
        dimensions: param.dimensions,
        arguments: resolved_parameters(&param.arguments, class, parent, types)?,
    })
}

fn resolve_name(name: &str, scope: &JavaScope, types: &Types<'_>) -> Option<String> {
    let Some((head, tail)) = name.split_once('.') else { return resolve_simple(name, scope, types) };
    match resolve_simple(head, scope, types) {
        Some(prefix) => Some(format!("{prefix}.{tail}")),
        None if types.contains_key(name) || head.starts_with(char::is_lowercase) => Some(name.to_owned()),
        None => None,
    }
}

fn resolve_simple(name: &str, scope: &JavaScope, types: &Types<'_>) -> Option<String> {
    let local = qualify(&scope.package, name);
    if scope.local_types.iter().any(|ty| ty == name) {
        return Some(local);
    }
    let mut explicit = scope.imports.iter().filter(|path| simple_name(path) == name);
    if let Some(first) = explicit.next() {
        return explicit.all(|path| path == first).then(|| first.clone());
    }
    if scope.static_imports.iter().any(|path| simple_name(path) == name) {
        return None;
    }
    if types.contains_key(&local) {
        return Some(local);
    }
    if scope.static_wildcard {
        return None;
    }
    resolve_on_demand(name, scope, types)
}

fn resolve_on_demand(name: &str, scope: &JavaScope, types: &Types<'_>) -> Option<String> {
    let java_lang = common_java_lang_type(name).then(|| qualify("java.lang", name));
    let mut candidates = scope.wildcards.iter().map(|package| qualify(package, name))
        .filter(|name| types.get(name).is_some_and(|entry| entry.is_none_or(|ty| ty.java.as_ref().is_some_and(|java| java.public))))
        .chain(java_lang);
    let first = candidates.next()?;
    candidates.all(|candidate| candidate == first).then_some(first)
}

fn qualify(package: &str, name: &str) -> String {
    if package.is_empty() { name.to_owned() } else { format!("{package}.{name}") }
}

fn simple_name(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

// The repository index has no JDK classpath. Cover common implicitly imported
// types in otherwise unambiguous signatures.
fn common_java_lang_type(name: &str) -> bool {
    matches!(name, "Boolean" | "Byte" | "Character" | "Class" | "Comparable" | "Double" | "Enum" | "Error"
        | "Exception" | "Float" | "Integer" | "Iterable" | "Long" | "Number" | "Object" | "Record" | "Runnable"
        | "RuntimeException" | "Short" | "String" | "StringBuilder" | "CharSequence" | "Thread" | "Throwable" | "Void")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::extract::extract_file;

    use super::*;

    fn flagged_methods(files: &[(&str, &str)]) -> Vec<String> {
        let types = files.iter().flat_map(|(file, source)| extract_file(Path::new(file), source)).collect();
        find(&SymbolIndex { types }).into_iter()
            .map(|finding| format!("{}.{}", finding.owner_type, finding.method)).collect()
    }

    #[test]
    fn flags_real_overrides_and_ignores_overloads_and_nontrivial_bodies() {
        let files = [
            ("Base.java", "package store; class Base {
                void reset(int value) {}
                void flush(String value) {}
                void save(String first, int second) {}
                void work() {}
            }"),
            ("Child.java", "package store; class Child extends Base {
                void reset(String value) {}
                @Override void flush(java.lang.String renamed) { throw new UnsupportedOperationException(); }
                void save(int first, String second) {}
                @Override void work() { super.work(); }
            }"),
        ];
        assert_eq!(flagged_methods(&files), ["Child.flush"]);
    }

    #[test]
    fn explicit_import_wins_over_same_package_and_wildcard_classes() {
        let files = [
            ("base/Parent.java", "package base; public class Parent { protected void chosen(int value) {} }"),
            ("child/Parent.java", "package child; public class Parent { public void unrelated(int value) {} }"),
            ("other/Parent.java", "package other; public class Parent { public void unrelated(int value) {} }"),
            ("Child.java", "package child; import base.Parent; import other.*;
                class Child extends Parent {
                    @Override protected void chosen(int renamed) {}
                    void unrelated(int value) {}
                }"),
        ];
        assert_eq!(flagged_methods(&files), ["Child.chosen"]);
    }

    #[test]
    fn resolves_wildcard_and_fully_qualified_parents_and_skips_ambiguity() {
        let files = [
            ("first/Parent.java", "package first; public class Parent { public void work() {} }"),
            ("second/Parent.java", "package second; public class Parent { public void unrelated() {} }"),
            ("Star.java", "package child; import first.*; class Star extends Parent { public void work() {} }"),
            ("Qualified.java", "package child; class Qualified extends first.Parent { public void work() {} }"),
            ("Unknown.java", "package child; import first.*; import second.*;
                class Unknown extends Parent { public void work() {} }"),
            ("External.java", "package child; import external.Parent;
                class External extends Parent { public void work() {} }"),
        ];
        assert_eq!(flagged_methods(&files), ["Star.work", "Qualified.work"]);
    }

    #[test]
    fn normalizes_array_varargs_and_imported_parameter_types() {
        let files = [
            ("Base.java", "package base; import java.util.List;
                public class Base {
                    public void array(String items[]) {}
                    public void list(List<String> items) {}
                    public void different(java.util.Date date) {}
                }"),
            ("Sub.java", "package child; import base.Base; import java.sql.Date;
                class Sub extends Base {
                    @Override public void array(String... names) {}
                    @Override public void list(java.util.List<java.lang.String> names) {}
                    public void different(Date date) {}
                }"),
        ];
        assert_eq!(flagged_methods(&files), ["Sub.array", "Sub.list"]);
    }

    #[test]
    fn checks_inheritance_modifiers() {
        let files = [
            ("Base.java", "package base; public class Base {
                static public void hidden() {}
                private void secret() {}
                void packageOnly() {}
                protected void inherited() {}
            }"),
            ("Sub.java", "package child; import base.Base; class Sub extends Base {
                public static void hidden() {}
                void secret() {}
                void packageOnly() {}
                @Override protected final void inherited() {}
            }"),
        ];
        assert_eq!(flagged_methods(&files), ["Sub.inherited"]);
    }
}
