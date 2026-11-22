use std::collections::HashMap;

use tree_sitter::Node;

use crate::lower::site::children;

use super::super::{syntax::descendants, ContractLanguage};

pub(super) struct Members<'src>(HashMap<&'src str, usize>);

impl Members<'_> {
    pub(super) fn get(&self, name: &str) -> Option<usize> {
        self.0.get(name).copied()
    }
    pub(super) fn keys(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.values().copied()
    }
}

pub(super) struct Allocations<'tree, 'src> {
    root: Node<'tree>,
    source: &'src [u8],
    language: ContractLanguage,
}

impl<'tree, 'src> Allocations<'tree, 'src> {
    pub(super) fn new(function: Node<'tree>, source: &'src [u8], language: ContractLanguage) -> Self {
        let root = std::iter::successors(Some(function), Node::parent).last().unwrap_or(function);
        Self { root, source, language }
    }

    pub(super) fn members(&self, value: Node<'_>) -> Option<Members<'src>> {
        let names = match self.language {
            ContractLanguage::Go => self.go_fields(value)?,
            ContractLanguage::Rust => rust_fields(value)?,
            ContractLanguage::Web => web_fields(value)?,
            ContractLanguage::Java | ContractLanguage::Kotlin => self.jvm_fields(value)?,
        };
        Some(Members(
            names
                .into_iter()
                .filter_map(|node| {
                    let name = node.utf8_text(self.source).ok()?;
                    (name != "__proto__").then_some((name, node.start_byte()))
                })
                .collect(),
        ))
    }

    fn declaration(&self, kind: &str, name: &str, at: usize) -> Option<Node<'tree>> {
        let mut candidates = descendants(self.root)
            .filter(|node| node.kind() == kind)
            .filter(|node| declaration_visible(*node, at))
            .filter(|node| {
                node.child_by_field_name("name").is_some_and(|part| part.utf8_text(self.source) == Ok(name))
            });
        let first = candidates.next()?;
        candidates.next().is_none().then_some(first)
    }

    fn go_fields<'node>(&self, value: Node<'node>) -> Option<Vec<Node<'node>>>
    where
        'tree: 'node,
    {
        let literal = if value.kind() == "unary_expression" && value.child(0).is_some_and(|op| op.kind() == "&") {
            value.child_by_field_name("operand")?
        } else {
            value
        };
        let ty = literal.child_by_field_name("type").filter(|_| literal.kind() == "composite_literal")?;
        let structure = match ty.kind() {
            "struct_type" => ty,
            "type_identifier" => self
                .declaration("type_spec", ty.utf8_text(self.source).ok()?, value.start_byte())?
                .child_by_field_name("type")?,
            _ => return None,
        };
        let fields = children(structure).find(|node| node.kind() == "field_declaration_list")?;
        Some(
            children(fields)
                .flat_map(|field| field.children_by_field_name("name", &mut field.walk()).collect::<Vec<_>>())
                .collect(),
        )
    }

    fn jvm_fields<'node>(&self, value: Node<'node>) -> Option<Vec<Node<'node>>>
    where
        'tree: 'node,
    {
        let ty = match self.language {
            ContractLanguage::Java if value.kind() == "object_creation_expression" => {
                let args = value.child_by_field_name("arguments")?;
                value
                    .child_by_field_name("type")
                    .filter(|ty| ty.kind() == "type_identifier" && args.named_child_count() == 0)?
            }
            ContractLanguage::Kotlin if value.kind() == "call_expression" => kotlin_constructor(value)?,
            _ => return None,
        };
        let declaration = self.declaration("class_declaration", ty.utf8_text(self.source).ok()?, value.start_byte())?;
        class_fields(declaration, self.language)
    }
}

fn declaration_visible(node: Node<'_>, at: usize) -> bool {
    std::iter::successors(node.parent(), Node::parent)
        .find(|scope| matches!(scope.kind(), "program" | "source_file" | "class_body" | "block"))
        .is_some_and(|scope| scope.byte_range().contains(&at))
}

fn rust_fields(value: Node<'_>) -> Option<Vec<Node<'_>>> {
    let body = value.child_by_field_name("body").filter(|_| value.kind() == "struct_expression")?;
    children(body)
        .map(|initializer| match initializer.kind() {
            "field_initializer" => initializer.child_by_field_name("field"),
            "shorthand_field_initializer" => initializer.named_child(0),
            _ => None,
        })
        .collect()
}

fn web_fields(value: Node<'_>) -> Option<Vec<Node<'_>>> {
    if value.kind() != "object" {
        return None;
    }
    children(value)
        .map(|item| match item.kind() {
            "pair" => item.child_by_field_name("key").filter(|key| key.kind() == "property_identifier"),
            "shorthand_property_identifier" => Some(item),
            _ => None,
        })
        .collect()
}

fn kotlin_constructor(node: Node<'_>) -> Option<Node<'_>> {
    let mut parts = children(node);
    let name = parts.next().filter(|node| node.kind() == "identifier")?;
    let args = parts.next().filter(|node| node.kind() == "value_arguments")?;
    (args.named_child_count() == 0 && parts.next().is_none()).then_some(name)
}

fn class_fields(node: Node<'_>, language: ContractLanguage) -> Option<Vec<Node<'_>>> {
    let body = children(node).find(|node| node.kind() == "class_body")?;
    if children(node).any(|part| !matches!(part.kind(), "class_body" | "identifier" | "modifiers")) {
        return None;
    }
    let mut names = Vec::new();
    for member in children(body) {
        match language {
            ContractLanguage::Java if member.kind() == "field_declaration" => {
                names.extend(java_members(member)?);
            }
            ContractLanguage::Kotlin if member.kind() == "property_declaration" => {
                names.push(kotlin_member(member)?);
            }
            _ => return None,
        }
    }
    Some(names)
}

fn java_members(node: Node<'_>) -> Option<Vec<Node<'_>>> {
    let static_member = children(node)
        .filter(|child| child.kind() == "modifiers")
        .any(|modifiers| modifiers.children(&mut modifiers.walk()).any(|modifier| modifier.kind() == "static"));
    if static_member {
        return Some(Vec::new());
    }
    children(node)
        .filter(|part| part.kind() == "variable_declarator")
        .map(|decl| {
            let name = decl.child_by_field_name("name")?;
            decl.child_by_field_name("value").is_none_or(plain_literal).then_some(name)
        })
        .collect()
}

fn kotlin_member(node: Node<'_>) -> Option<Node<'_>> {
    let target = children(node).find(|node| node.kind() == "variable_declaration")?;
    let plain = children(node).all(|child| child == target || child.kind() == "modifiers" || plain_literal(child));
    // A custom accessor or delegate can observe a store directly.
    plain.then(|| target.named_child(0)).flatten()
}

fn plain_literal(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "null_literal"
            | "decimal_integer_literal"
            | "number_literal"
            | "true"
            | "false"
            | "boolean_literal"
            | "identifier"
    )
}
