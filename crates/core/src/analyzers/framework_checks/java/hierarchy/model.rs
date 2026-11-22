use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use autoreview_langsupport::{parser_for, Language};
use autoreview_schema::Location;
use tree_sitter::Node;

use super::super::super::{children, descendants, parse_source};
use super::super::{class_members, modifiers, source_files, Java};

pub(in crate::analyzers::framework_checks::java) const TX: &str =
    "org.springframework.transaction.annotation.Transactional";

#[derive(Default)]
pub(in crate::analyzers::framework_checks::java) struct Hierarchy {
    // A duplicate source declaration has no unique owner.
    names: HashMap<String, Option<(String, usize)>>,
    declarations: HashMap<String, HashMap<usize, Type>>,
    aspectj_paths: HashSet<String>,
}

pub(in crate::analyzers::framework_checks::java) struct Type {
    pub name: String,
    pub package: String,
    imports: HashMap<String, String>,
    wildcards: Vec<String>,
    pub superclass: Option<String>,
    pub interfaces: Vec<String>,
    pub is_interface: bool,
    pub methods: Vec<Method>,
    pub transaction: Option<Transaction>,
}

pub(in crate::analyzers::framework_checks::java) struct Method {
    pub name: String,
    pub parameters: Vec<Option<String>>,
    pub varargs: bool,
    pub generic: bool,
    pub visibility: Visibility,
    pub is_static: bool,
    pub return_type: Option<String>,
    pub is_final: bool,
    pub location: Location,
    pub start: usize,
    pub transaction: Option<Transaction>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::analyzers::framework_checks::java) enum Visibility {
    Private,
    Package,
    Inherited,
}

pub(in crate::analyzers::framework_checks::java) struct Transaction {
    pub location: Location,
    pub default_settings: bool,
}

impl Hierarchy {
    pub fn load(root: &Path) -> Self {
        let mut index = Self::default();
        let Some(mut parser) = parser_for(Language::Java) else {
            return index;
        };
        for path in source_files(root) {
            let Some((source, tree)) = parse_source(&mut parser, &path) else {
                continue;
            };
            let name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            index.insert_source(&Java::new(&name, &source, &tree));
        }
        index
    }

    fn insert_source(&mut self, java: &Java<'_>) {
        const ENABLE: &str =
            "org.springframework.transaction.annotation.EnableTransactionManagement";
        for declaration in descendants(java.root).filter(|node| is_type(*node)) {
            if java
                .annotation(declaration, ENABLE)
                .and_then(|node| java.field(node, "arguments"))
                .is_some_and(|args| args.contains("ASPECTJ"))
            {
                self.aspectj_paths.insert(java.path.into());
            }
            let owner = Type::parse(java, declaration);
            let start = declaration.start_byte();
            self.names
                .entry(owner.name.clone())
                .and_modify(|entry| *entry = None)
                .or_insert_with(|| Some((java.path.into(), start)));
            self.declarations
                .entry(java.path.into())
                .or_default()
                .insert(start, owner);
        }
    }

    pub fn owner(&self, java: &Java<'_>, declaration: Node<'_>) -> Option<&Type> {
        self.declarations
            .get(java.path)?
            .get(&declaration.start_byte())
    }

    pub fn aspectj_enabled(&self, java: &Java<'_>) -> bool {
        self.aspectj_paths.contains(java.path)
    }

    pub fn named(&self, owner: &Type, name: &str) -> Option<&Type> {
        let (path, start) = self.names.get(&self.canonical(owner, name)?)?.as_ref()?;
        self.declarations.get(path)?.get(start)
    }

    pub fn canonical(&self, owner: &Type, raw: &str) -> Option<String> {
        if matches!(
            raw,
            "boolean" | "byte" | "short" | "char" | "int" | "long" | "float" | "double"
        ) {
            return Some(raw.into());
        }
        let first = raw.split('.').next()?;
        if let Some(import) = owner.imports.get(first) {
            return Some(format!("{import}{}", &raw[first.len()..]));
        }
        self.source_name(owner, raw).or_else(|| standard_type(raw))
    }

    fn source_name(&self, owner: &Type, raw: &str) -> Option<String> {
        for name in owner.local_names(raw) {
            if self.names.contains_key(&name) {
                return Some(name);
            }
        }
        if raw.contains('.') && self.names.contains_key(raw) {
            return Some(raw.into());
        }
        let mut found = owner
            .wildcard_names(raw)
            .filter(|name| self.names.contains_key(name));
        let name = found.next()?;
        found.next().is_none().then_some(name)
    }
}

fn standard_type(raw: &str) -> Option<String> {
    if matches!(
        raw,
        "String"
            | "Object"
            | "Boolean"
            | "Byte"
            | "Short"
            | "Character"
            | "Integer"
            | "Long"
            | "Float"
            | "Double"
    ) {
        return Some(format!("java.lang.{raw}"));
    }
    raw.contains('.').then(|| raw.to_owned())
}

impl Type {
    fn local_names(&self, raw: &str) -> [String; 2] {
        [qualified(&self.name, raw), qualified(&self.package, raw)]
    }

    fn wildcard_names<'a>(&'a self, raw: &'a str) -> impl Iterator<Item = String> + 'a {
        self.wildcards
            .iter()
            .map(move |package| qualified(package, raw))
    }

    pub fn direct_superclass<'a>(&self, hierarchy: &'a Hierarchy) -> Option<&'a Type> {
        hierarchy.named(self, self.superclass.as_deref()?)
    }

    pub fn takes_precedence_over(&self, other: &Type) -> bool {
        !self.is_interface && other.is_interface
    }

    fn parse(java: &Java<'_>, declaration: Node<'_>) -> Self {
        let is_interface = declaration.kind() == "interface_declaration";
        Self {
            name: type_key(java, declaration)
                .unwrap_or_else(|| format!("{}@{}", java.path, declaration.start_byte())),
            package: java.package.to_owned(),
            imports: java
                .imports
                .iter()
                .map(|(&name, &path)| (name.into(), path.into()))
                .collect(),
            wildcards: java.wildcards.iter().map(|name| (*name).into()).collect(),
            superclass: declaration
                .child_by_field_name("superclass")
                .and_then(|superclass| type_children(superclass).next())
                .map(|parent| java.text(parent).to_owned()),
            interfaces: children(declaration)
                .filter(|child| matches!(child.kind(), "super_interfaces" | "extends_interfaces"))
                .flat_map(children)
                .filter(|list| list.kind() == "type_list")
                .flat_map(type_children)
                .map(|parent| java.text(parent).to_owned())
                .collect(),
            is_interface,
            methods: class_members(declaration)
                .filter(|node| node.kind() == "method_declaration")
                .filter_map(|node| Method::parse(java, node, is_interface))
                .collect(),
            transaction: transaction(java, declaration),
        }
    }
}

impl Method {
    pub(in crate::analyzers::framework_checks::java) fn parse(java: &Java<'_>, node: Node<'_>, interface: bool) -> Option<Self> {
        let params = node.child_by_field_name("parameters")?;
        let parameters = type_children(params)
            .filter(|part| part.kind() != "receiver_parameter")
            .map(|part| parameter_type(java, part))
            .collect();
        Some(Self {
            name: java.field(node, "name")?.into(),
            parameters,
            varargs: children(params).any(|part| part.kind() == "spread_parameter"),
            generic: node.child_by_field_name("type_parameters").is_some(),
            visibility: visibility(node, interface),
            is_static: modifiers(node).any(|modifier| modifier.kind() == "static"),
            return_type: node.child_by_field_name("type").and_then(|node| simple_type(java, node)),
            is_final: modifiers(node).any(|modifier| modifier.kind() == "final"),
            location: java.location(node),
            start: node.start_byte(),
            transaction: transaction(java, node),
        })
    }
}

pub(in crate::analyzers::framework_checks::java) fn type_children(
    node: Node<'_>,
) -> impl Iterator<Item = Node<'_>> {
    children(node).filter(|node| !matches!(node.kind(), "line_comment" | "block_comment"))
}

pub(in crate::analyzers::framework_checks::java) fn simple_type(
    java: &Java<'_>,
    node: Node<'_>,
) -> Option<String> {
    matches!(
        node.kind(),
        "type_identifier"
            | "scoped_type_identifier"
            | "integral_type"
            | "floating_point_type"
            | "boolean_type"
    )
    .then(|| java.text(node).to_owned())
}

pub(in crate::analyzers::framework_checks::java) fn parameter_type(
    java: &Java<'_>,
    parameter: Node<'_>,
) -> Option<String> {
    if parameter.kind() != "formal_parameter"
        || parameter.child_by_field_name("dimensions").is_some()
    {
        return None;
    }
    simple_type(java, parameter.child_by_field_name("type")?)
}

fn visibility(node: Node<'_>, interface: bool) -> Visibility {
    if modifiers(node).any(|modifier| modifier.kind() == "private") {
        Visibility::Private
    } else if interface
        || modifiers(node).any(|modifier| matches!(modifier.kind(), "public" | "protected"))
    {
        Visibility::Inherited
    } else {
        Visibility::Package
    }
}

fn transaction(java: &Java<'_>, node: Node<'_>) -> Option<Transaction> {
    let marker = java.annotation(node, TX)?;
    Some(Transaction {
        location: java.location(marker),
        default_settings: marker
            .child_by_field_name("arguments")
            .is_none_or(|args| type_children(args).next().is_none()),
    })
}

pub(in crate::analyzers::framework_checks::java) fn type_key(java: &Java<'_>, declaration: Node<'_>) -> Option<String> {
    let mut hierarchy = vec![java.field(declaration, "name")?];
    let mut parent = declaration.parent()?;
    while matches!(parent.kind(), "class_body" | "interface_body") {
        let enclosing = parent.parent().filter(|node| is_type(*node))?;
        hierarchy.push(java.field(enclosing, "name")?);
        parent = enclosing.parent()?;
    }
    if parent.kind() != "program" {
        return None;
    }
    hierarchy.reverse();
    Some(qualified(java.package, &hierarchy.join(".")))
}

fn is_type(node: Node<'_>) -> bool {
    matches!(node.kind(), "class_declaration" | "interface_declaration")
}

fn qualified(package: &str, name: &str) -> String {
    if package.is_empty() {
        name.to_owned()
    } else {
        format!("{package}.{name}")
    }
}
