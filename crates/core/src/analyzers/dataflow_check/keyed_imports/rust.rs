use std::collections::HashSet;

use super::{children, issue, text, AnalysisCoverageIssue, ImportSource, KeyedExport, KeyedImport, Language, Node, Path, PathBuf, Resolver};

impl ImportSource<'_> {
    pub(super) fn rust(&self, coverage: &mut Vec<AnalysisCoverageIssue>) -> Vec<KeyedImport> {
        let path = self.path;
        let resolver = self.resolver;
        let Some(dir) = module_dir(path) else {
            return Vec::new();
        };
        let references = module_references(self.node, self.source);
        children(self.node)
            .into_iter()
            .flat_map(|node| match node.kind() {
                "mod_item" if node.child_by_field_name("body").is_none() => {
                    let name = node
                        .child_by_field_name("name")
                        .and_then(|name| text(name, self.source));
                    name.filter(|name| references.as_ref().is_none_or(|refs| refs.contains(name)))
                        .and_then(|name| {
                        let local = format!("{name}::");
                        let binding = (!has_attributes(node)).then(|| rust_module(resolver, &dir.join(name)))
                            .flatten().and_then(|file| resolver.binding(&file, &local, None, None, true));
                        if binding.is_none() { record_unresolved(self, node, &local, coverage); }
                        binding
                    }).into_iter().collect()
                }
                "use_declaration" => rust_uses(self, &dir, node, coverage),
                _ => Vec::new(),
            })
            .collect()
    }
}

// External `mod` declarations do not themselves reference the module. Keep
// every identifier elsewhere, including imports and macro arguments, as an
// over-approximation of potential references. Do not prune malformed sources.
fn module_references<'a>(root: Node<'_>, source: &'a str) -> Option<HashSet<&'a str>> {
    if root.has_error() { return None; }
    let mut names = HashSet::new();
    let mut pending = children(root);
    while let Some(node) = pending.pop() {
        if node.kind() == "mod_item" && node.child_by_field_name("body").is_none() { continue; }
        if matches!(node.kind(), "identifier" | "type_identifier" | "field_identifier") {
            if let Some(name) = text(node, source) { names.insert(name); }
        }
        pending.extend(children(node));
    }
    Some(names)
}

impl ImportSource<'_> {
    pub(super) fn rust_exports(&self) -> Vec<KeyedExport> {
        children(self.node).into_iter().filter(|node| node.kind() == "use_declaration")
            .flat_map(|node| self.reexports(node)).collect()
    }

    fn reexports(&self, node: Node<'_>) -> Vec<KeyedExport> {
        let visibility = children(node).into_iter().find(|part| part.kind() == "visibility_modifier")
            .and_then(|part| text(part, self.source));
        let Some(visibility) = visibility else { return Vec::new() };
        if !matches!(visibility, "pub" | "pub(crate)") || has_attributes(node) { return vec![KeyedExport::Unknown]; }
        let Some(usage) = node.child_by_field_name("argument").and_then(|part| text(part, self.source)) else { return vec![KeyedExport::Unknown] };
        let paths = rust_paths(usage.trim());
        if paths.is_empty() { return vec![KeyedExport::Unknown]; }
        paths.into_iter().map(|path| self.reexport(&path)).collect()
    }

    fn reexport(&self, usage: &str) -> KeyedExport {
        let (qualified, alias) = usage.split_once(" as ").map_or((usage, None), |(path, alias)| (path.trim(), Some(alias.trim())));
        let Some((module, member)) = qualified.rsplit_once("::") else { return KeyedExport::Unknown };
        if member == "self" || member.contains(['{', '}']) { return KeyedExport::Unknown; }
        let Some(path) = reexport_module(self.resolver, self.path, module).and_then(|file| self.resolver.relative(&file)) else {
            return KeyedExport::Unknown;
        };
        if member == "*" && alias.is_none() { KeyedExport::Star { path } }
        else if valid_name(member) && alias.is_none_or(valid_name) {
            KeyedExport::Named { exported: alias.unwrap_or(member).to_owned(), local: member.to_owned(), path: Some(path) }
        } else { KeyedExport::Unknown }
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
}

fn reexport_module(resolver: &Resolver<'_>, path: &Path, module: &str) -> Option<PathBuf> {
    if module == "crate" { crate_root(resolver, path) }
    else if module.starts_with("crate::") { verified_namespace(resolver, path, module) }
    else { relative_module(resolver, path, module) }
}

fn relative_module(resolver: &Resolver<'_>, path: &Path, module: &str) -> Option<PathBuf> {
    crate_root(resolver, path)?;
    let mut parts = module.split("::");
    let mut current = match parts.next()? {
        "self" => path.to_path_buf(),
        "super" => parent_module(resolver, path)?,
        _ => return None,
    };
    for name in parts {
        current = match name {
            "super" => parent_module(resolver, &current)?,
            "self" | "crate" => return None,
            _ if valid_name(name) => declared_module(resolver, &current, name)?,
            _ => return None,
        };
    }
    Some(current)
}

// Walk a unique declared module edge backwards; filenames alone do not
// establish a Rust module relationship or select among crate roots.
fn parent_module(resolver: &Resolver<'_>, path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    if matches!(stem, "lib" | "main") { return None; }
    let name = if stem == "mod" { path.parent()?.file_name()?.to_str()? } else { stem };
    let module_directory = module_dir(path)?;
    let parent = module_directory.parent()?;
    let candidates = [parent.join("lib.rs"), parent.join("main.rs"), parent.with_extension("rs"), parent.join("mod.rs")];
    let existing: Vec<_> = candidates.into_iter().filter(|candidate|
        candidate.is_file() && resolver.relative(candidate).is_some()).collect();
    let [candidate] = existing.as_slice() else { return None };
    let current = resolver.relative(path)?;
    (declared_module(resolver, candidate, name).and_then(|child| resolver.relative(&child)).as_deref() == Some(&current))
        .then(|| candidate.clone())
}

fn rust_uses(
    input: &ImportSource<'_>, dir: &Path, node: Node<'_>, coverage: &mut Vec<AnalysisCoverageIssue>,
) -> Vec<KeyedImport> {
    let ImportSource { resolver, path, source, .. } = *input;
    let conditional = has_attributes(node);
    let Some(usage) = node
        .child_by_field_name("argument")
        .and_then(|node| text(node, source))
    else {
        return Vec::new();
    };
    let paths = rust_paths(usage.trim());
    paths.into_iter().flat_map(|usage| {
        let (qualified, alias) = usage.split_once(" as ")
            .map_or((usage.as_str(), None), |(path, alias)| (path.trim(), Some(alias.trim())));
        let Some((module, member)) = qualified.rsplit_once("::") else { return Vec::new() };
        let mut bindings = Vec::new();
        if let Some(file) = (!conditional).then(|| resolve_use_module(resolver, path, dir, module)).flatten() {
            if let Some(binding) = resolver.binding(&file, alias.unwrap_or(member), Some(member), None, true) {
                bindings.push(binding);
            }
        }
        let (module_path, default) = if member == "self" {
            (module, module.rsplit("::").next().unwrap_or(module))
        } else { (qualified, member) };
        let local = format!("{}::", alias.unwrap_or(default));
        let namespace = (!conditional).then(|| reexport_module(resolver, path, module_path)).flatten();
        if let Some(file) = namespace {
            if let Some(binding) = resolver.binding(&file, &local, None, None, true) { bindings.push(binding); }
        } else if is_relative(module_path) {
            record_unresolved(input, node, &local, coverage);
        }
        if bindings.is_empty() && is_relative(module_path) && member != "self" {
            record_unresolved(input, node, alias.unwrap_or(member), coverage);
        }
        bindings
    }).collect()

}

fn is_relative(path: &str) -> bool {
    matches!(path.split("::").next(), Some("crate" | "self" | "super"))
}

fn resolve_use_module(resolver: &Resolver<'_>, path: &Path, dir: &Path, module: &str) -> Option<PathBuf> {
    if is_relative(module) { reexport_module(resolver, path, module) }
    else { rust_use_file(resolver, path, dir, module) }
}

fn record_unresolved(input: &ImportSource<'_>, node: Node<'_>, local: &str, coverage: &mut Vec<AnalysisCoverageIssue>) {
    if !namespace_called(input.node, local, input.source) { return; }
    let mut record = issue(input.resolver.relative(input.path).as_deref(), "unresolved_rust_module_import", "dependency",
        "A used Rust import or associated call could not be resolved safely; cross-file call-sequence analysis may be incomplete.");
    record.line = Some(node.start_position().row as u32 + 1);
    if !coverage.contains(&record) { coverage.push(record); }
}

fn namespace_called(root: Node<'_>, prefix: &str, source: &str) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == "call_expression" && node.child_by_field_name("function")
            .and_then(|function| text(function, source)).is_some_and(|name| {
                if prefix.ends_with("::") { name.starts_with(prefix) }
                else { name == prefix || name.strip_prefix(prefix).is_some_and(|tail| tail.starts_with("::<")) }
            }) { return true; }
        pending.extend(children(node));
    }
    false
}

// Resolve only declared external modules with a unique conventional path.
// Attributes can change the selected file or build configuration.
fn verified_namespace(resolver: &Resolver<'_>, path: &Path, qualified: &str) -> Option<PathBuf> {
    let suffix = qualified.strip_prefix("crate::")?;
    let parts: Vec<_> = suffix.split("::").collect();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty() || !part.chars().all(|ch| ch.is_alphanumeric() || ch == '_')) { return None; }
    let mut current = crate_root(resolver, path)?;
    for name in parts { current = declared_module(resolver, &current, name)?; }
    Some(current)
}

fn crate_root(resolver: &Resolver<'_>, path: &Path) -> Option<PathBuf> {
    let dir = path.ancestors().skip(1).find(|parent| parent.starts_with(&resolver.root)
        && (parent.join("lib.rs").is_file() || parent.join("main.rs").is_file()))?;
    let roots: Vec<_> = [dir.join("lib.rs"), dir.join("main.rs")].into_iter().filter(|file| file.is_file()).collect();
    let [root] = roots.as_slice() else { return None };
    let root_relative = resolver.relative(root)?;
    let mut current = path.to_path_buf();
    for _ in 0..128 {
        if resolver.relative(&current).as_deref() == Some(&root_relative) { return Some(root.clone()); }
        current = parent_module(resolver, &current)?;
    }
    None
}

fn declared_module(resolver: &Resolver<'_>, current: &Path, name: &str) -> Option<PathBuf> {
    let parsed = resolver.cache.get(current, Language::Rust)?;
    if parsed.1.root_node().has_error() { return None; }
    let modules: Vec<_> = children(parsed.1.root_node()).into_iter().filter(|node| node.kind() == "mod_item"
        && node.child_by_field_name("body").is_none()
        && node.child_by_field_name("name").and_then(|part| text(part, &parsed.0)) == Some(name)).collect();
    let [module] = modules.as_slice() else { return None };
    (!has_attributes(*module)).then(|| rust_module(resolver, &module_dir(current)?.join(name))).flatten()
}

fn has_attributes(node: Node<'_>) -> bool {
    std::iter::successors(node.prev_named_sibling(), |node| node.prev_named_sibling())
        .take_while(|node| matches!(node.kind(), "attribute_item" | "line_comment" | "block_comment"))
        .any(|node| node.kind() == "attribute_item")
}

fn rust_paths(usage: &str) -> Vec<String> {
    if let Some((prefix, members)) = usage.split_once("::{") {
        return members
            .strip_suffix('}')
            .into_iter()
            .flat_map(|members| members.split(','))
            .map(str::trim)
            .filter(|member| !member.is_empty() && !member.contains(['{', '}']))
            .map(|member| format!("{prefix}::{member}"))
            .collect();
    }
    vec![usage.to_owned()]
}

fn rust_use_file(
    resolver: &Resolver<'_>,
    path: &Path,
    dir: &Path,
    module: &str,
) -> Option<PathBuf> {
    let crate_dir = path
        .ancestors()
        .skip(1)
        .find(|parent| {
            parent.starts_with(&resolver.root)
                && (parent.join("lib.rs").is_file() || parent.join("main.rs").is_file())
        })
        .unwrap_or(dir);
    let (base, module) = match module.split_once("::") {
        Some(("crate", rest)) => (crate_dir, rest),
        Some(("self", rest)) => (dir, rest),
        Some(("super", rest)) => (dir.parent()?, rest),
        _ => (crate_dir, module),
    };
    if !module
        .split("::")
        .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_alphanumeric() || ch == '_'))
    {
        return None;
    }
    rust_module(resolver, &base.join(module.replace("::", "/")))
}

fn module_dir(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let name = path.file_stem()?.to_str()?;
    if matches!(name, "mod" | "lib" | "main") {
        Some(parent.to_owned())
    } else {
        Some(parent.join(name))
    }
}

fn rust_module(resolver: &Resolver<'_>, base: &Path) -> Option<PathBuf> {
    let candidates = [base.with_extension("rs"), base.join("mod.rs")];
    let mut files = candidates
        .into_iter()
        .filter(|path| resolver.relative(path).is_some() && path.is_file());
    let file = files.next()?;
    files.next().is_none().then_some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_references_keep_imports_and_macro_arguments_but_not_declarations() {
        let source = "mod unused; mod called; mod imported; mod macro_arg; use crate::imported::run; fn f() { called::run(); macro_call!(macro_arg::run()); }";
        let mut parser = autoreview_langsupport::parser_for(Language::Rust).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let names = module_references(tree.root_node(), source).unwrap();
        assert!(!names.contains("unused"));
        for name in ["called", "imported", "macro_arg"] { assert!(names.contains(name), "{name}"); }
        let invalid = "mod unused; fn incomplete(";
        let tree = parser.parse(invalid, None).unwrap();
        assert!(module_references(tree.root_node(), invalid).is_none());
    }
}
