mod constructor;
mod executor_shutdown;
mod hibernate;
mod hierarchy;
mod otel;
mod pagination;
mod spring;

use std::{collections::HashMap, path::Path};

use autoreview_langsupport::{parser_for, Language};
use autoreview_schema::AgentFinding;
use tree_sitter::{Node, Tree};

use super::{children, descendants, location, parse_source, text};
use hierarchy::Hierarchy;

pub(super) use constructor::RULE as CONSTRUCTOR_RULE;
pub(super) use executor_shutdown::RULE as EXECUTOR_SHUTDOWN_RULE;
pub(super) use hibernate::RULE as HIBERNATE_RULE;
pub(super) use otel::RULE as OTEL_RULE;
pub(super) use pagination::RULE as PAGINATION_RULE;
pub(super) use spring::RULE as SPRING_RULE;

pub(super) fn run(root: &Path, paths: &[String]) -> Vec<AgentFinding> {
    let mut findings = Vec::new();
    let Some(mut parser) = parser_for(Language::Java) else {
        return findings;
    };
    let mut entity_index = None;
    let mut hierarchy = None;
    for path in paths.iter().filter(|path| path.ends_with(".java")) {
        let Some((source, tree)) = parse_source(&mut parser, &root.join(path)) else {
            continue;
        };
        let java = Java::new(path, &source, &tree);
        let types = hierarchy.get_or_insert_with(|| Hierarchy::load(root));
        findings.extend(constructor::findings(&java, types));
        findings.extend(spring::findings(&java, types));
        if source.contains("addShutdownHook") {
            findings.extend(executor_shutdown::findings(&java));
        }
        if source.contains("unpaged") {
            findings.extend(pagination::findings(&java));
        }
        if source.contains("makeCurrent") {
            findings.extend(otel::findings(&java));
        }
        if source.contains("getResultList") {
            let entities = entity_index.get_or_insert_with(|| hibernate::index(root));
            findings.extend(hibernate::findings(&java, entities));
        }
    }
    findings
}

pub(super) struct Java<'a> {
    path: &'a str,
    source: &'a str,
    root: Node<'a>,
    package: &'a str,
    imports: HashMap<&'a str, &'a str>,
    wildcards: Vec<&'a str>,
}

impl<'a> Java<'a> {
    fn new(path: &'a str, source: &'a str, tree: &'a Tree) -> Self {
        let root = tree.root_node();
        let mut java = Self {
            path,
            source,
            root,
            package: "",
            imports: HashMap::new(),
            wildcards: Vec::new(),
        };
        for node in children(root) {
            match node.kind() {
                "package_declaration" => {
                    java.package = node.named_child(0).map_or("", |name| java.text(name))
                }
                "import_declaration" => java.import(node),
                _ => {}
            }
        }
        java
    }

    fn import(&mut self, node: Node<'a>) {
        let name = self
            .text(node)
            .trim_start_matches("import")
            .trim_end_matches(';')
            .trim();
        if name.starts_with("static ") {
            return;
        }
        if let Some(pkg) = name.strip_suffix(".*") {
            self.wildcards.push(pkg);
        } else if let Some((_, short)) = name.rsplit_once('.') {
            self.imports.insert(short, name);
        }
    }

    fn text(&self, node: Node<'_>) -> &'a str {
        text(node, self.source)
    }

    fn field(&self, node: Node<'_>, name: &str) -> Option<&'a str> {
        node.child_by_field_name(name).map(|node| self.text(node))
    }

    fn resolves(&self, actual: &str, qualified: &str) -> bool {
        if actual == qualified {
            return true;
        }
        let Some((package, name)) = qualified.rsplit_once('.') else {
            return false;
        };
        name == actual
            && self.imports.get(actual).map_or_else(
                || self.wildcards.contains(&package),
                |import| *import == qualified,
            )
    }

    fn annotation(&self, node: Node<'a>, qualified: &str) -> Option<Node<'a>> {
        annotations(node).find(|annotation| {
            self.field(*annotation, "name")
                .is_some_and(|name| self.resolves(name, qualified))
        })
    }

    fn location(&self, node: Node<'_>) -> autoreview_schema::Location {
        location(self.path, node, self.source)
    }
}

fn annotations(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    children(node)
        .find(|node| node.kind() == "modifiers")
        .into_iter()
        .flat_map(children)
        .filter(|node| matches!(node.kind(), "annotation" | "marker_annotation"))
}

fn class_members(class: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    class
        .child_by_field_name("body")
        .into_iter()
        .flat_map(children)
}

fn containing<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    std::iter::successors(node.parent(), |node| node.parent()).find(|node| node.kind() == kind)
}

fn modifiers(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let Some(modifiers) = children(node).find(|child| child.kind() == "modifiers") else {
        return Vec::new().into_iter();
    };
    let mut cursor = modifiers.walk();
    modifiers.children(&mut cursor).collect::<Vec<_>>().into_iter()
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
            if kind.is_dir() && source_directory(&entry.file_name()) {
                pending.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "java") {
                java.push(path);
            }
        }
    }
    java
}

fn source_directory(name: &std::ffi::OsStr) -> bool {
    !name.to_string_lossy().starts_with('.')
        && !matches!(
            name.to_str(),
            Some("target" | "build" | "node_modules" | "vendor" | "out")
        )
}
