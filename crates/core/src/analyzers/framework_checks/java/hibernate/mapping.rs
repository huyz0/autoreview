use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use autoreview_langsupport::{parser_for, Language};
use autoreview_schema::Location;
use tree_sitter::Node;

use super::super::{annotations, children, class_members, Java};

pub(in crate::analyzers::framework_checks::java) type Entities = HashMap<String, Entity>;

pub(in crate::analyzers::framework_checks::java) struct Entity {
    pub simple_name: String,
    pub collections: HashMap<String, Location>,
}

pub(in crate::analyzers::framework_checks::java) fn index(root: &Path) -> Entities {
    let mut entities = HashMap::new();
    let mut ambiguous = HashSet::new();
    let Some(mut parser) = parser_for(Language::Java) else {
        return entities;
    };
    for path in source_files(root) {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !source.contains("Entity") {
            continue;
        }
        let Some(tree) = parser.parse(&source, None) else {
            continue;
        };
        if tree.root_node().has_error() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let java = Java::new(&relative, &source, &tree);
        for (name, mapping) in children(java.root).filter_map(|node| entity(&java, node)) {
            if entities.insert(name.clone(), mapping).is_some() {
                ambiguous.insert(name);
            }
        }
    }
    entities.retain(|name, _| !ambiguous.contains(name));
    entities
}

fn source_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut pending = vec![root.to_owned()];
    let mut java = Vec::new();
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with('.')
                    && !matches!(
                        name.to_str(),
                        Some("target" | "build" | "node_modules" | "vendor" | "out")
                    )
                {
                    pending.push(path);
                }
            } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "java") {
                java.push(path);
            }
        }
    }
    java
}

fn entity(java: &Java<'_>, declaration: Node<'_>) -> Option<(String, Entity)> {
    if declaration.kind() != "class_declaration" {
        return None;
    }
    let annotation = persistence_annotation(java, declaration, "Entity")?;
    if annotation
        .child_by_field_name("arguments")
        .is_some_and(|args| args.named_child_count() != 0)
    {
        return None;
    }
    let name = java.field(declaration, "name")?;
    let qualified = if java.package.is_empty() {
        name.to_owned()
    } else {
        format!("{}.{name}", java.package)
    };
    let mut collections = HashMap::new();
    for member in class_members(declaration) {
        if let Some(getter) = lazy_getter(java, member) {
            collections.insert(getter, java.location(member));
        }
    }
    Some((
        qualified,
        Entity {
            simple_name: name.to_owned(),
            collections,
        },
    ))
}

fn lazy_getter(java: &Java<'_>, member: Node<'_>) -> Option<String> {
    if !matches!(member.kind(), "field_declaration" | "method_declaration")
        || has_fetch_override(java, member)
    {
        return None;
    }
    let mapping = ["OneToMany", "ManyToMany", "ElementCollection"]
        .into_iter()
        .find_map(|name| persistence_annotation(java, member, name))?;
    if mapping
        .child_by_field_name("arguments")
        .is_some_and(|args| java.text(args).contains("EAGER"))
    {
        return None;
    }
    if member.kind() == "method_declaration" {
        let name = java.field(member, "name")?;
        return name.starts_with("get").then(|| name.to_owned());
    }
    let field = member.child_by_field_name("declarator")?;
    let name = java.field(field, "name")?;
    let initial = name.chars().next()?;
    Some(format!(
        "get{}{}",
        initial.to_uppercase(),
        &name[initial.len_utf8()..]
    ))
}

fn has_fetch_override(java: &Java<'_>, member: Node<'_>) -> bool {
    annotations(member).any(|annotation| {
        java.field(annotation, "name").is_some_and(|name| {
            java.resolves(name, "org.hibernate.annotations.Fetch")
                || java.resolves(name, "org.hibernate.annotations.BatchSize")
        })
    })
}

fn persistence_annotation<'a>(java: &Java<'a>, node: Node<'a>, name: &str) -> Option<Node<'a>> {
    ["jakarta.persistence", "javax.persistence"]
        .into_iter()
        .find_map(|package| java.annotation(node, &format!("{package}.{name}")))
}
