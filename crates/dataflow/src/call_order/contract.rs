use tree_sitter::Node;

use crate::lower::site::children;

mod imports;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractMode {
    MustUse,
    MustComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractLanguage {
    Go,
    Java,
    Kotlin,
    Rust,
    Web,
}

#[derive(Debug, Clone)]
pub struct ResultContract {
    pub mode: ContractMode,
    pub language: ContractLanguage,
    pub import: String,
    pub methods: Vec<String>,
    pub borrows: Vec<String>,
    pub acquire_awaited: bool,
    pub complete_observed: bool,
    /// Sources are obtained from verified TypeScript callable types instead
    /// of matching a named API. Only valid for web must-use contracts.
    pub typed_promise_factory: bool,
}

/// Resolve explicit imports and fully qualified Java calls. A name-only
/// match cannot prove that an API owns a result or requires completion.
pub(super) struct ApiMatcher<'a> {
    contract: &'a ResultContract,
    names: Vec<String>,
}

impl<'a> ApiMatcher<'a> {
    pub(super) fn new(contract: &'a ResultContract, function: Node<'_>, source: &[u8]) -> Self {
        let root = std::iter::successors(Some(function), Node::parent)
            .last()
            .unwrap_or(function);
        let names = match contract.language {
            ContractLanguage::Go => qualified_names(go_qualifiers(root, source, &contract.import), ".", contract),
            ContractLanguage::Java => qualified_names(java_qualifiers(root, function, source, &contract.import), ".", contract),
            ContractLanguage::Kotlin => imports::kotlin_names(root, source, contract),
            ContractLanguage::Rust => imports::rust_names(function, source, contract),
            ContractLanguage::Web => imports::web_names(root, source, contract),
        };
        Self { contract, names }
    }

    pub(super) fn mode(&self) -> ContractMode {
        self.contract.mode
    }

    pub(super) fn supports_local_handles(&self, function: Node<'_>, source: &[u8]) -> bool {
        if self.mode() != ContractMode::MustComplete {
            return true;
        }
        let owner = function.parent().and_then(|body| body.parent());
        let mut pending = vec![function];
        while let Some(node) = pending.pop() {
            if may_store_handle(node, owner, source) || returns_tuple(node) || using_declaration(node) {
                return false;
            }
            pending.extend(children(node));
        }
        true
    }

    pub(super) fn matches(&self, call: &str, bound: impl Fn(&str) -> bool) -> bool {
        let call = call.strip_prefix("::").unwrap_or(call);
        let root = call.split(['.', ':']).next().unwrap_or(call);
        self.names.iter().any(|name| name == call) && !bound(root)
    }
}

fn qualified_names(qualifiers: Vec<String>, separator: &str, contract: &ResultContract) -> Vec<String> {
    qualifiers.into_iter().flat_map(|qualifier| {
        contract.methods.iter().map(move |method| format!("{qualifier}{separator}{method}"))
    }).collect()
}

fn using_declaration(node: Node<'_>) -> bool {
    node.kind() == "using_declaration" || node.children(&mut node.walk()).any(|token| token.kind() == "using")
}

fn returns_tuple(node: Node<'_>) -> bool {
    node.kind() == "return_statement"
        && children(node)
            .find(|child| child.kind() == "expression_list")
            .is_some_and(|list| children(list).nth(1).is_some())
}

fn may_store_handle(node: Node<'_>, owner: Option<Node<'_>>, source: &[u8]) -> bool {
    if node.kind() == "send_statement" {
        return true;
    }
    if !matches!(
        node.kind(),
        "assignment_statement" | "assignment_expression" | "assignment"
    ) {
        return false;
    }
    let Some(target) = node.child_by_field_name("left") else {
        return false;
    };
    if target.kind() == "expression_list" {
        return children(target).any(|part| nonlocal_target(part, owner, source));
    }
    nonlocal_target(target, owner, source)
}

fn nonlocal_target(target: Node<'_>, owner: Option<Node<'_>>, source: &[u8]) -> bool {
    matches!(
        target.kind(),
        "field_access" | "selector_expression" | "index_expression" | "array_access" | "member_expression" | "subscript_expression" | "field_expression" | "navigation_expression"
    ) || owner
        .zip(target.utf8_text(source).ok())
        .is_some_and(|(owner, name)| type_body_shadows(owner, source, name))
}

fn go_qualifiers(root: Node<'_>, source: &[u8], package: &str) -> Vec<String> {
    children(root)
        .filter(|node| node.kind() == "import_declaration")
        .flat_map(children)
        .flat_map(|node| {
            if node.kind() == "import_spec_list" {
                children(node).collect()
            } else {
                vec![node]
            }
        })
        .filter_map(|spec| {
            let path = spec.child_by_field_name("path")?.utf8_text(source).ok()?;
            if path.trim_matches(['"', '`']) != package {
                return None;
            }
            let alias = spec
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source).ok())
                .or_else(|| package.rsplit('/').next())?;
            (!matches!(alias, "_" | ".")).then(|| alias.to_owned())
        })
        .collect()
}

fn java_qualifiers(root: Node<'_>, function: Node<'_>, source: &[u8], import: &str) -> Vec<String> {
    let Some((_, short)) = import.rsplit_once('.') else {
        return Vec::new();
    };
    let Some(owner) = function
        .parent()
        .and_then(|body| body.parent())
        .filter(|owner| is_simple_owner(*owner))
    else {
        return Vec::new();
    };
    // A member can shadow the imported name or the root of a qualified call.
    let mut qualifiers = Vec::new();
    if let Some((prefix, _)) = import.split_once('.') {
        if !type_body_shadows(owner, source, prefix) {
            qualifiers.push(import.to_owned());
        }
    }
    if explicitly_imports(root, source, import) && !type_body_shadows(owner, source, short) {
        qualifiers.push(short.to_owned());
    }
    qualifiers
}

fn is_simple_owner(owner: Node<'_>) -> bool {
    owner
        .parent()
        .is_some_and(|parent| parent.kind() == "program")
        && owner.child_by_field_name("superclass").is_none()
        && !children(owner)
            .any(|child| matches!(child.kind(), "super_interfaces" | "extends_interfaces"))
}

fn explicitly_imports(root: Node<'_>, source: &[u8], import: &str) -> bool {
    children(root)
        .filter(|node| node.kind() == "import_declaration")
        .filter_map(|node| node.utf8_text(source).ok())
        .any(|text| {
            text.strip_prefix("import")
                .is_some_and(|name| name.trim_end_matches(';').trim() == import)
        })
}

fn type_body_shadows(owner: Node<'_>, source: &[u8], name: &str) -> bool {
    owner
        .child_by_field_name("body")
        .into_iter()
        .flat_map(children)
        .any(|node| {
            let named = |node: Node<'_>| {
                node.child_by_field_name("name")
                    .is_some_and(|part| part.utf8_text(source) == Ok(name))
            };
            match node.kind() {
                "field_declaration" => children(node)
                    .any(|child| child.kind() == "variable_declarator" && named(child)),
                "class_declaration"
                | "enum_declaration"
                | "interface_declaration"
                | "record_declaration" => named(node),
                _ => false,
            }
        })
}
