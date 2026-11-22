use std::collections::{HashMap, HashSet};

use super::{
    children, descendants, field_text, member, text, transformation, unwrap, Go, Java, JavaScript,
    Kotlin, Language, Node, Rust, Tsx, TypeScript,
};
use crate::analyzers::rust_review::syntax::Source;

pub(super) struct Strings<'t, 's> {
    language: Language,
    source: &'s str,
    bindings: HashMap<&'s str, Vec<Binding<'t>>>,
    writes: HashMap<&'s str, Vec<usize>>,
    go_aliases: HashSet<&'s str>,
    shadowed_jvm_string: bool,
    rust: Option<Source<'t, 's>>,
}

struct Binding<'t> {
    declaration: Node<'t>,
    scope: Node<'t>,
}

impl<'t, 's> Strings<'t, 's> {
    pub(super) fn new(language: Language, root: Node<'t>, source: &'s str) -> Self {
        let mut index = Self {
            language,
            source,
            bindings: HashMap::new(),
            writes: HashMap::new(),
            go_aliases: HashSet::new(),
            shadowed_jvm_string: false,
            rust: (language == Rust).then(|| Source::new(root, source)),
        };
        if language != Rust {
            for node in descendants(root) {
                index.index(node);
            }
        }
        index
    }

    fn index(&mut self, node: Node<'t>) {
        if let Some(name) = declared_name(node) {
            let scope = binding_scope(node);
            self.bindings
                .entry(text(name, self.source))
                .or_default()
                .push(Binding {
                    declaration: node,
                    scope,
                });
        } else if matches!(self.language, JavaScript | TypeScript | Tsx) {
            for name in super::super::web::binding_nodes(node) {
                if name
                    .parent()
                    .is_none_or(|parent| parent.kind() != "formal_parameters")
                {
                    self.bindings
                        .entry(text(name, self.source))
                        .or_default()
                        .push(Binding {
                            declaration: name,
                            scope: binding_scope(node),
                        });
                }
            }
        }
        if matches!(
            node.kind(),
            "assignment_expression" | "augmented_assignment_expression" | "update_expression"
        ) {
            let target = node
                .child_by_field_name("left")
                .or_else(|| node.child_by_field_name("argument"));
            for target in target
                .into_iter()
                .flat_map(descendants)
                .filter(|node| node.kind() == "identifier")
            {
                self.writes
                    .entry(text(target, self.source))
                    .or_default()
                    .push(node.start_byte());
            }
        }
        if node.kind() == "import_spec"
            && field_text(node, "path", self.source)
                .is_some_and(|path| path.trim_matches(['"', '\x60']) == "strings")
        {
            self.go_aliases
                .insert(field_text(node, "name", self.source).unwrap_or("strings"));
        }
        self.shadowed_jvm_string |= shadows_string(node, self.source);
    }

    pub(super) fn is_transformation(&self, call: Node<'t>) -> bool {
        if self.language == Go {
            return self.go_call(call);
        }
        let Some((receiver, method)) = member(call, self.source, self.language) else {
            return false;
        };
        transformation(self.language, method)
            && self.known_string(receiver, 0)
            && self.ordinary_arguments(call, method)
    }

    fn go_call(&self, call: Node<'t>) -> bool {
        let Some(selector) = call.child_by_field_name("function") else {
            return false;
        };
        let Some(object) = selector.child_by_field_name("operand") else {
            return false;
        };
        let name = text(object, self.source);
        const METHODS: &str = "Trim TrimLeft TrimRight TrimSpace TrimPrefix TrimSuffix Replace ReplaceAll ToLower ToUpper ToTitle ToValidUTF8 Repeat Clone";
        self.go_aliases.contains(name)
            && self.visible(name, object).is_none()
            && !object
                .ancestors()
                .filter(|node| {
                    matches!(
                        node.kind(),
                        "function_declaration" | "method_declaration" | "func_literal"
                    )
                })
                .any(|function| go_shadows(function, name, self.source))
            && field_text(selector, "field", self.source).is_some_and(|method| {
                METHODS
                    .split_ascii_whitespace()
                    .any(|known| known == method)
            })
    }

    fn known_string(&self, node: Node<'t>, depth: usize) -> bool {
        if depth >= 8 {
            return false;
        }
        let node = unwrap(node);
        match node.kind() {
            "string"
            | "template_string"
            | "string_literal"
            | "raw_string_literal"
            | "multiline_string_literal" => {
                self.language != Rust || text(node, self.source).starts_with(['"', 'r'])
            }
            "identifier" => self.string_binding(node, depth + 1),
            "method_invocation" | "call_expression" => self.string_call(node, depth + 1),
            _ => false,
        }
    }

    fn string_call(&self, node: Node<'t>, depth: usize) -> bool {
        if let Some(source) = &self.rust {
            if rust_string_constructor(source, node) {
                return true;
            }
        }
        member(node, self.source, self.language).is_some_and(|(receiver, method)| {
            let returns_string = if self.language == Rust {
                matches!(method, "to_string" | "to_owned")
                    || (!method.starts_with("strip_") && transformation(Rust, method))
            } else {
                transformation(self.language, method)
            };
            returns_string && self.known_string(receiver, depth)
        })
    }

    fn string_binding(&self, node: Node<'t>, depth: usize) -> bool {
        let binding = self
            .rust
            .as_ref()
            .and_then(|source| source.declaration(node))
            .or_else(|| self.visible(text(node, self.source), node));
        let Some(binding) = binding else { return false };
        if let Some(ty) = binding_type(binding, self.language) {
            if self.language != Java || text(ty, self.source) != "var" {
                return self.string_type(ty);
            }
        }
        if matches!(self.language, JavaScript | TypeScript | Tsx)
            && self
                .writes
                .get(text(node, self.source))
                .is_some_and(|writes| {
                    writes
                        .iter()
                        .any(|at| *at > binding.start_byte() && *at < node.start_byte())
                })
        {
            return false;
        }
        initializer(binding, self.language).is_some_and(|value| self.known_string(value, depth))
    }

    fn visible(&self, name: &str, site: Node<'_>) -> Option<Node<'t>> {
        self.bindings
            .get(name)?
            .iter()
            .filter(|entry| {
                contains(entry.scope, site)
                    && entry.declaration.id() != site.id()
                    && (self.language != Java
                        || entry.declaration.start_byte() < site.start_byte()
                        || entry
                            .declaration
                            .parent()
                            .is_some_and(|parent| parent.kind() == "field_declaration"))
            })
            .min_by_key(|entry| {
                (
                    entry.scope.end_byte() - entry.scope.start_byte(),
                    entry.declaration.start_byte() > site.start_byte(),
                    std::cmp::Reverse(entry.declaration.start_byte()),
                )
            })
            .map(|entry| entry.declaration)
    }

    fn string_type(&self, ty: Node<'t>) -> bool {
        let spelling = text(ty, self.source).trim();
        match self.language {
            Java => {
                spelling == "java.lang.String" || spelling == "String" && !self.shadowed_jvm_string
            }
            Kotlin => {
                matches!(
                    spelling.trim_end_matches('?'),
                    "kotlin.String" | "kotlin.CharSequence"
                ) || matches!(spelling.trim_end_matches('?'), "String" | "CharSequence")
                    && !self.shadowed_jvm_string
            }
            JavaScript | TypeScript | Tsx => web_string_type(spelling),
            Rust => self
                .rust
                .as_ref()
                .is_some_and(|source| rust_string(source, ty)),
            Go => false,
        }
    }

    fn ordinary_arguments(&self, call: Node<'t>, method: &str) -> bool {
        if self.language == Kotlin {
            return !descendants(call)
                .skip(1)
                .any(|arg| matches!(arg.kind(), "lambda_literal" | "anonymous_function"));
        }
        if !matches!(self.language, JavaScript | TypeScript | Tsx)
            || !matches!(method, "replace" | "replaceAll")
        {
            return true;
        }
        let Some(args) = call.child_by_field_name("arguments") else {
            return false;
        };
        let mut args = children(args).filter(|node| node.kind() != "comment");
        args.next()
            .is_some_and(|pattern| pattern.kind() == "regex" || self.known_string(pattern, 0))
            && args
                .next()
                .is_some_and(|replacement| self.known_string(replacement, 0))
    }
}

fn declared_name(node: Node<'_>) -> Option<Node<'_>> {
    let name = match node.kind() {
        "variable_declarator"
        | "formal_parameter"
        | "parameter_declaration"
        | "var_spec"
        | "const_spec"
        | "function_declaration"
        | "function_expression"
        | "class_declaration" => node.child_by_field_name("name"),
        "required_parameter" | "optional_parameter" => node.child_by_field_name("pattern"),
        "parameter" | "variable_declaration" if node.child_by_field_name("type").is_none() => {
            node.named_child(0)
        }
        "identifier"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "formal_parameters") =>
        {
            Some(node)
        }
        "arrow_function" => node.child_by_field_name("parameter"),
        _ => None,
    }?;
    (name.kind() == "identifier").then_some(name)
}

fn web_string_type(spelling: &str) -> bool {
    let mut has_string = false;
    spelling
        .trim_start_matches(':')
        .split('|')
        .all(|part| match part.trim() {
            "string" => {
                has_string = true;
                true
            }
            "null" | "undefined" => true,
            _ => false,
        })
        && has_string
}

fn rust_string_constructor<'t>(source: &Source<'t, '_>, call: Node<'t>) -> bool {
    let Some(function) = call.child_by_field_name("function") else {
        return false;
    };
    let Some(owner) = function.child_by_field_name("path") else {
        return false;
    };
    function
        .child_by_field_name("name")
        .is_some_and(|name| matches!(source.text(name), "from" | "new" | "with_capacity"))
        && rust_string(source, owner)
}

fn binding_scope(node: Node<'_>) -> Node<'_> {
    if node.kind() == "arrow_function" {
        return node;
    }
    let function_scoped = node.kind() == "variable_declarator"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "variable_declaration");
    node.ancestors()
        .find(|scope| {
            if function_scoped {
                matches!(
                    scope.kind(),
                    "program"
                        | "function_declaration"
                        | "function_expression"
                        | "method_definition"
                        | "arrow_function"
                )
            } else {
                lexical_scope(*scope)
            }
        })
        .unwrap_or(node)
}

fn lexical_scope(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "program"
            | "source_file"
            | "block"
            | "statement_block"
            | "class_body"
            | "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "method_declaration"
            | "constructor_declaration"
            | "lambda_literal"
            | "lambda_expression"
            | "catch_clause"
            | "for_statement"
            | "for_in_statement"
    )
}

fn contains(scope: Node<'_>, site: Node<'_>) -> bool {
    scope.start_byte() <= site.start_byte() && site.end_byte() <= scope.end_byte()
}

fn binding_type(node: Node<'_>, language: Language) -> Option<Node<'_>> {
    let ty = match (language, node.kind()) {
        (Java, "variable_declarator") => node.parent()?.child_by_field_name("type")?,
        (Kotlin, "parameter" | "variable_declaration") => node.named_child(1)?,
        _ => node.child_by_field_name("type")?,
    };
    Some(ty)
}

fn initializer(node: Node<'_>, language: Language) -> Option<Node<'_>> {
    if language == Kotlin && node.kind() == "variable_declaration" {
        let property = node
            .parent()
            .filter(|parent| parent.kind() == "property_declaration")?;
        return children(property).find(|child| {
            child.start_byte() >= node.end_byte() && child.kind() != "property_delegate"
        });
    }
    node.child_by_field_name("value")
}

fn shadows_string(node: Node<'_>, source: &str) -> bool {
    match node.kind() {
        "class_declaration"
        | "interface_declaration"
        | "record_declaration"
        | "type_alias"
        | "typealias_declaration" => field_text(node, "name", source)
            .is_some_and(|name| matches!(name, "String" | "CharSequence")),
        "type_parameter" => node
            .named_child(0)
            .is_some_and(|name| text(name, source) == "String"),
        "import_declaration" | "import" | "import_header" => {
            let path = text(node, source).trim_end_matches(';').trim();
            (path.ends_with(".String") || path.ends_with(" as String"))
                && !matches!(path, "import java.lang.String" | "import kotlin.String")
        }
        _ => false,
    }
}

fn go_shadows(function: Node<'_>, name: &str, source: &str) -> bool {
    descendants(function).any(|node| {
        let left = match node.kind() {
            "short_var_declaration" | "range_clause" => node.child_by_field_name("left"),
            "parameter_declaration"
            | "variadic_parameter_declaration"
            | "var_spec"
            | "const_spec" => Some(node),
            _ => None,
        };
        left.into_iter()
            .flat_map(children)
            .any(|binding| binding.kind() == "identifier" && text(binding, source) == name)
    })
}

fn rust_string<'t>(source: &Source<'t, '_>, mut ty: Node<'t>) -> bool {
    while ty.kind() == "reference_type" {
        let Some(inner) = ty.child_by_field_name("type") else {
            return false;
        };
        ty = inner;
    }
    ty.kind() == "primitive_type" && source.text(ty) == "str"
        || source.unshadowed_prelude(ty, "String")
        || matches!(
            source.external_path(ty).as_deref(),
            Some("std::string::String" | "alloc::string::String")
        )
}

trait Ancestors<'t> {
    fn ancestors(self) -> impl Iterator<Item = Node<'t>>;
}

impl<'t> Ancestors<'t> for Node<'t> {
    fn ancestors(self) -> impl Iterator<Item = Node<'t>> {
        std::iter::successors(self.parent(), |node| node.parent())
    }
}
