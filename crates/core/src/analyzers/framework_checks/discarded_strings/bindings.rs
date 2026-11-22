mod syntax;

use syntax::{
    binding_scope, binding_type, contains, declared_name, go_shadows, initializer, shadows_string,
    web_string_type, Ancestors,
};

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
            if source.constructs_string(node) {
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
                .is_some_and(|source| source.is_standard_string(ty)),
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
