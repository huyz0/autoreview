use std::path::{Path, PathBuf};

use autoreview_dataflow::call_order::KeyedImport;
use autoreview_langsupport::{language_for_path, Language};
use tree_sitter::Node;

use super::{children, text, Resolver};

pub(super) fn go(
    resolver: &Resolver<'_>,
    path: &Path,
    node: Node<'_>,
    source: &str,
) -> Vec<KeyedImport> {
    let parent = path.parent().unwrap_or(&resolver.root);
    let mut imports = Vec::new();
    let package = go_package(node, source);
    for file in go_files(resolver, parent) {
        if file != path && resolver.go_package_of(&file).as_deref() == package {
            imports.extend(resolver.binding(&file, "", None, None, false));
        }
    }
    let Some(module) = autoreview_archgraph::discover_go_module_path(&resolver.root) else {
        return imports;
    };
    let mut nodes = children(node);
    while let Some(node) = nodes.pop() {
        match node.kind() {
            "import_declaration" | "import_spec_list" => nodes.extend(children(node)),
            "import_spec" => imports.extend(go_import(resolver, node, source, &module)),
            _ => {}
        }
    }
    imports
}

impl Resolver<'_> {
    fn go_package_of(&self, file: &Path) -> Option<String> {
        let parsed = self.cache.get(file, Language::Go)?;
        go_package(parsed.1.root_node(), &parsed.0).map(str::to_owned)
    }
}

fn go_import(
    resolver: &Resolver<'_>,
    node: Node<'_>,
    source: &str,
    module: &str,
) -> Vec<KeyedImport> {
    let Some(target) = node
        .child_by_field_name("path")
        .and_then(|path| text(path, source))
        .and_then(unquote)
    else {
        return Vec::new();
    };
    let Some(suffix) = target
        .strip_prefix(module)
        .and_then(|target| target.strip_prefix('/'))
    else {
        return Vec::new();
    };
    let alias = node
        .child_by_field_name("name")
        .and_then(|name| text(name, source));
    if matches!(alias, Some("_" | ".")) {
        return Vec::new();
    }
    go_files(resolver, &resolver.root.join(suffix))
        .into_iter()
        .filter_map(|file| {
            let package = resolver.go_package_of(&file)?;
            let prefix = format!("{}.", alias.unwrap_or(&package));
            resolver.binding(&file, &prefix, None, None, true)
        })
        .collect()
}

fn go_files(resolver: &Resolver<'_>, dir: &Path) -> Vec<PathBuf> {
    resolver
        .files(dir, "go")
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.ends_with("_test.go"))
        })
        .collect()
}

fn go_package<'src>(node: Node<'_>, source: &'src str) -> Option<&'src str> {
    let clause = children(node)
        .into_iter()
        .find(|node| node.kind() == "package_clause")?;
    text(clause.named_child(0)?, source)
}

pub(super) fn web(
    resolver: &Resolver<'_>,
    path: &Path,
    node: Node<'_>,
    source: &str,
) -> Vec<KeyedImport> {
    children(node)
        .into_iter()
        .filter(|node| node.kind() == "import_statement")
        .flat_map(|node| {
            let target = node
                .child_by_field_name("source")
                .and_then(|value| text(value, source))
                .and_then(unquote);
            let Some(file) = target.and_then(|target| web_file(resolver, path.parent()?, target))
            else {
                return Vec::new();
            };
            web_bindings(resolver, node, source, &file)
        })
        .collect()
}

fn web_bindings(
    resolver: &Resolver<'_>,
    node: Node<'_>,
    source: &str,
    file: &Path,
) -> Vec<KeyedImport> {
    let Some(clause) = children(node)
        .into_iter()
        .find(|node| node.kind() == "import_clause")
    else {
        return Vec::new();
    };
    children(clause)
        .into_iter()
        .flat_map(|part| match part.kind() {
            "named_imports" => children(part)
                .into_iter()
                .filter_map(|item| {
                    let name = text(item.child_by_field_name("name")?, source)?;
                    let alias = item
                        .child_by_field_name("alias")
                        .and_then(|alias| text(alias, source))
                        .unwrap_or(name);
                    resolver.binding(file, alias, Some(name), None, true)
                })
                .collect(),
            "identifier" => text(part, source)
                .and_then(|local| resolver.binding(file, local, Some("default"), None, true))
                .into_iter()
                .collect(),
            "namespace_import" => part
                .named_child(0)
                .and_then(|name| text(name, source))
                .and_then(|name| resolver.binding(file, &format!("{name}."), None, None, true))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn web_file(resolver: &Resolver<'_>, parent: &Path, target: &str) -> Option<PathBuf> {
    if !target.starts_with("./") && !target.starts_with("../") {
        return None;
    }
    let base = parent.join(target);
    let extensions = ["ts", "tsx", "js", "mjs", "cjs", "mts", "cts"];
    let mut candidates = vec![base.clone()];
    if base.extension().is_none() {
        candidates.extend(extensions.iter().map(|ext| base.with_extension(ext)));
        candidates.extend(
            extensions
                .iter()
                .map(|ext| base.join(format!("index.{ext}"))),
        );
    } else if base.extension().is_some_and(|ext| ext == "js") {
        candidates.push(base.with_extension("ts"));
        candidates.push(base.with_extension("tsx"));
    }
    candidates.into_iter().find(|path| {
        resolver.relative(path).is_some()
            && path.is_file()
            && language_for_path(path).is_some_and(|lang| {
                matches!(
                    lang,
                    Language::JavaScript | Language::TypeScript | Language::Tsx
                )
            })
    })
}

fn unquote(value: &str) -> Option<&str> {
    let delimiter = value.chars().next()?;
    if !matches!(delimiter, '\'' | '"' | '`') {
        return None;
    }
    let content = value.strip_prefix(delimiter)?.strip_suffix(delimiter)?;
    (!content.contains(['\\', '\n', '\r'])).then_some(content)
}

pub(super) fn rust(
    resolver: &Resolver<'_>,
    path: &Path,
    node: Node<'_>,
    source: &str,
) -> Vec<KeyedImport> {
    let Some(dir) = module_dir(path) else {
        return Vec::new();
    };
    children(node)
        .into_iter()
        .flat_map(|node| match node.kind() {
            "mod_item" if node.child_by_field_name("body").is_none() => {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|name| text(name, source));
                name.and_then(|name| {
                    rust_module(resolver, &dir.join(name)).and_then(|file| {
                        resolver.binding(&file, &format!("{name}::"), None, None, true)
                    })
                })
                .into_iter()
                .collect()
            }
            "use_declaration" => rust_uses(resolver, path, &dir, node, source),
            _ => Vec::new(),
        })
        .collect()
}

fn rust_uses(
    resolver: &Resolver<'_>,
    path: &Path,
    dir: &Path,
    node: Node<'_>,
    source: &str,
) -> Vec<KeyedImport> {
    let Some(usage) = node
        .child_by_field_name("argument")
        .and_then(|node| text(node, source))
    else {
        return Vec::new();
    };
    let paths = rust_paths(usage.trim());
    paths
        .into_iter()
        .filter_map(|usage| {
            let (qualified, alias) = usage
                .split_once(" as ")
                .map_or((usage.as_str(), None), |(path, alias)| {
                    (path.trim(), Some(alias.trim()))
                });
            let (module, member) = qualified.rsplit_once("::")?;
            let file = rust_use_file(resolver, path, dir, module)?;
            resolver.binding(&file, alias.unwrap_or(member), Some(member), None, true)
        })
        .collect()
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
